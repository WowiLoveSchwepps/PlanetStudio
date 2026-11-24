//! Décompression LZHAM « alpha » (`LZHAM_DLL_VERSION` 0x1008), en Rust pur.
//!
//! Les chunks de terrain de PlanetSide 2 (`.cnk0` à `.cnk5`) sont compressés par cette version
//! de LZHAM, dans une enveloppe zlib (`5E D1` : méthode 14 = LZHAM, fenêtre de 2^20 octets). Ce
//! module traduit le décodeur de référence (github.com/richgel999/lzham_alpha, licence MIT) en
//! mode « sortie non tamponnée » : la taille décompressée est connue, tout est écrit directement
//! dans le tampon de sortie. La version 1.0 de LZHAM (2015) n'est pas compatible.
//!
//! Format du flux (bits lus du poids fort au poids faible) :
//! - en-tête zlib (2 octets) : `CMF & 15` = 14, fenêtre = `(CMF >> 4) + 15` bits ;
//! - 2 bits de configuration : mise à jour rapide des tables, codes « polaires » ;
//! - des blocs, chacun annoncé par 2 bits : synchronisation (0), compressé (1), brut (2) ou fin (3) ;
//! - après la fin, alignement sur l'octet puis Adler-32 des données (32 bits), vérifiée ici.
//!
//! Un bloc compressé démarre un codeur arithmétique binaire (probabilités sur 11 bits) pour les
//! décisions (littéral ou correspondance, types de répétition), avec pour contexte l'état de
//! l'analyseur et l'octet précédent. Littéraux, longueurs et distances passent par des codes
//! préfixes « quasi adaptatifs » (Huffman ou polaires) : leurs longueurs sont recalculées à partir
//! des fréquences observées, à un rythme qui ralentit peu à peu. Les littéraux ont 64 tables,
//! choisies par les 3 bits de poids fort des deux octets précédents.

use anyhow::{bail, ensure, Result};

// ------------------------------------------------------------------ constantes du format

const MIN_MATCH_LEN: u32 = 2;
const MAX_MATCH_LEN: u32 = 257;
const NUM_STATES: usize = 12;
const NUM_LIT_STATES: u32 = 7;
const NUM_LIT_PRED_BITS: u32 = 6;
const NUM_DELTA_LIT_PRED_BITS: u32 = 6;
const NUM_IS_MATCH_CONTEXT_BITS: u32 = 6;
const LZX_NUM_SECONDARY_LENGTHS: u32 = 249;
const NUM_HUGE_MATCH_CODES: u32 = 1;
const LZX_NUM_SPECIAL_LENGTHS: u32 = 2;
const LZX_LOWEST_USABLE_MATCH_SLOT: u32 = 1;
const LZX_MAX_POSITION_SLOTS: usize = 128;
/// méthode « LZHAM » dans l'en-tête zlib
const ZLIB_METHOD_LZHAM: u32 = 14;

const LITERAL_NEXT_STATE: [u8; 24] = [0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 4, 5, 7, 7, 7, 7, 7, 7, 7, 10, 10, 10, 10, 10];
const HUGE_MATCH_BASE_LEN: [u32; 4] = [
    MAX_MATCH_LEN + 1,
    MAX_MATCH_LEN + 1 + 256,
    MAX_MATCH_LEN + 1 + 256 + 1024,
    MAX_MATCH_LEN + 1 + 256 + 1024 + 4096,
];
const HUGE_MATCH_CODE_LEN: [u32; 4] = [8, 10, 12, 16];

const ARITH_MIN_LEN: u32 = 0x0100_0000;
const ARITH_PROB_BITS: u32 = 11;
const ARITH_PROB_SCALE: u32 = 1 << ARITH_PROB_BITS;
const ARITH_PROB_MOVE_BITS: u32 = 5;

const MAX_CODE_SIZE: usize = 16;
const TABLE_BITS: u32 = 11;

// ------------------------------------------------------------------ lecture des bits

/// Flux de bits, poids fort d'abord ; au-delà de la fin, on lit des zéros (comme la référence).
struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    #[inline]
    fn peek(&self, n: u32) -> u32 {
        debug_assert!(n <= 32);
        if n == 0 {
            return 0;
        }
        let byte = self.pos >> 3;
        let mut w = [0u8; 8];
        if byte + 8 <= self.d.len() {
            w.copy_from_slice(&self.d[byte..byte + 8]);
        } else if byte < self.d.len() {
            let k = self.d.len() - byte;
            w[..k].copy_from_slice(&self.d[byte..]);
        }
        let v = u64::from_be_bytes(w) << (self.pos & 7);
        (v >> (64 - n)) as u32
    }
    #[inline]
    fn get(&mut self, n: u32) -> u32 {
        let v = self.peek(n);
        self.pos += n as usize;
        v
    }
    fn align(&mut self) {
        self.pos = (self.pos + 7) & !7;
    }
    fn overrun(&self) -> bool {
        self.pos > self.d.len() * 8 + 64
    }
}

// ------------------------------------------------------------------ codeur arithmétique binaire

#[derive(Clone, Copy)]
struct BitModel(u16);

impl Default for BitModel {
    fn default() -> Self {
        BitModel(1 << (ARITH_PROB_BITS - 1))
    }
}

struct Arith {
    value: u32,
    length: u32,
}

impl Arith {
    fn start(bits: &mut Bits) -> Self {
        let mut value = 0u32;
        for _ in 0..4 {
            value = (value << 8) | bits.get(8);
        }
        Arith { value, length: u32::MAX }
    }

    #[inline]
    fn bit(&mut self, bits: &mut Bits, m: &mut BitModel) -> u32 {
        while self.length < ARITH_MIN_LEN {
            self.value = (self.value << 8) | bits.get(8);
            self.length <<= 8;
        }
        let x = m.0 as u32 * (self.length >> ARITH_PROB_BITS);
        if self.value >= x {
            m.0 -= m.0 >> ARITH_PROB_MOVE_BITS;
            self.value -= x;
            self.length -= x;
            1
        } else {
            m.0 += ((ARITH_PROB_SCALE - m.0 as u32) >> ARITH_PROB_MOVE_BITS) as u16;
            self.length = x;
            0
        }
    }
}

// ------------------------------------------------------------------ longueurs des codes

/// Nombre de bits pour écrire `v` (0 pour 0).
#[inline]
fn total_bits(v: u32) -> u32 {
    32 - v.leading_zeros()
}

/// Symboles de fréquence non nulle triés par fréquence croissante, à fréquence égale par numéro
/// (le tri par base de la référence est stable). Les fréquences nulles ont une longueur 0.
fn sorted_syms(freq: &[u16], sizes: &mut [u8]) -> Vec<(u32, u16)> {
    let mut syms: Vec<(u32, u16)> = Vec::with_capacity(freq.len());
    for (i, &f) in freq.iter().enumerate() {
        if f == 0 {
            sizes[i] = 0;
        } else {
            syms.push((f as u32, i as u16));
        }
    }
    syms.sort_by_key(|s| s.0);
    syms
}

/// Longueurs de Huffman (algorithme en place de Moffat et Katajainen, comme la référence).
fn huffman_code_sizes(freq: &[u16], sizes: &mut [u8]) -> u32 {
    let syms = sorted_syms(freq, sizes);
    let n = syms.len();
    if n == 0 {
        return 0;
    }
    if n == 1 {
        sizes[syms[0].1 as usize] = 1;
        return 1;
    }
    let mut a: Vec<i64> = syms.iter().map(|s| s.0 as i64).collect();
    // premier passage : pointeurs vers les parents
    a[0] += a[1];
    let (mut root, mut leaf) = (0usize, 2usize);
    for next in 1..n - 1 {
        if leaf >= n || a[root] < a[leaf] {
            a[next] = a[root];
            a[root] = next as i64;
            root += 1;
        } else {
            a[next] = a[leaf];
            leaf += 1;
        }
        if leaf >= n || (root < next && a[root] < a[leaf]) {
            a[next] += a[root];
            a[root] = next as i64;
            root += 1;
        } else {
            a[next] += a[leaf];
            leaf += 1;
        }
    }
    // profondeurs des nœuds internes
    a[n - 2] = 0;
    for next in (0..n - 2).rev() {
        a[next] = a[a[next] as usize] + 1;
    }
    // profondeurs des feuilles
    let (mut avbl, mut used, mut dpth) = (1i64, 0i64, 0i64);
    let mut root = n as i64 - 2;
    let mut next = n as i64 - 1;
    while avbl > 0 {
        while root >= 0 && a[root as usize] == dpth {
            used += 1;
            root -= 1;
        }
        while avbl > used {
            a[next as usize] = dpth;
            next -= 1;
            avbl -= 1;
        }
        avbl = 2 * used;
        dpth += 1;
        used = 0;
    }
    let mut max = 0;
    for i in 0..n {
        let len = a[i] as u32;
        max = max.max(len);
        sizes[syms[i].1 as usize] = len.min(255) as u8;
    }
    max
}

/// Longueurs des codes « polaires » : fréquences arrondies à la puissance de 2 inférieure, puis
/// doublées, des plus fréquentes aux moins fréquentes, jusqu'à remplir l'arbre.
fn polar_code_sizes(freq: &[u16], sizes: &mut [u8]) -> u32 {
    let syms = sorted_syms(freq, sizes);
    let n = syms.len();
    if n == 0 {
        return 0;
    }
    if n == 1 {
        sizes[syms[0].1 as usize] = 1;
        return 1;
    }
    let mut tmp = vec![0u32; n];
    let (mut orig_total, mut cur_total) = (0u32, 0u32);
    for i in 0..n {
        let f = syms[n - 1 - i].0;
        orig_total += f;
        let adj = 1 << (total_bits(f) - 1);
        tmp[i] = adj;
        cur_total += adj;
    }
    let mut tree_total = 1u32 << (total_bits(orig_total) - 1);
    if tree_total < orig_total {
        tree_total <<= 1;
    }
    let mut start = 0;
    while cur_total < tree_total && start < n {
        for i in start..n {
            let f = tmp[i];
            if cur_total + f <= tree_total {
                tmp[i] += f;
                cur_total += f;
                if cur_total == tree_total {
                    break;
                }
            } else {
                start = i + 1;
            }
        }
    }
    let tree_bits = total_bits(tree_total);
    let mut max = 0;
    for i in 0..n {
        let size = tree_bits - total_bits(tmp[i]);
        max = max.max(size);
        sizes[syms[n - 1 - i].1 as usize] = size as u8;
    }
    max
}

/// Ramène les codes à `max` bits au plus (technique de LHArc, comme la référence).
fn limit_code_sizes(sizes: &mut [u8], max: usize) -> Result<()> {
    const MAX_EVER: usize = 34;
    let mut num = [0u32; MAX_EVER + 1];
    let mut should = false;
    for &c in sizes.iter() {
        ensure!((c as usize) <= MAX_EVER, "LZHAM : code trop long");
        num[c as usize] += 1;
        should |= c as usize > max;
    }
    if !should {
        return Ok(());
    }
    let mut next_sorted = [0u32; MAX_EVER + 1];
    let mut ofs = 0;
    for i in 1..=MAX_EVER {
        next_sorted[i] = ofs;
        ofs += num[i];
    }
    if ofs < 2 {
        return Ok(());
    }
    ensure!(ofs <= 1 << max, "LZHAM : trop de symboles pour la longueur maximale");
    for i in max + 1..=MAX_EVER {
        num[max] += num[i];
    }
    let mut total: u32 = (1..=max).rev().map(|i| num[i] << (max - i)).sum();
    if total == 1 << max {
        return Ok(());
    }
    while total != 1 << max {
        num[max] -= 1;
        let mut i = max - 1;
        loop {
            ensure!(i > 0, "LZHAM : réduction des codes impossible");
            if num[i] > 0 {
                num[i] -= 1;
                num[i + 1] += 2;
                break;
            }
            i -= 1;
        }
        total -= 1;
    }
    let mut new_sizes = Vec::with_capacity(ofs as usize);
    for (i, &n) in num.iter().enumerate().take(max + 1).skip(1) {
        new_sizes.extend(std::iter::repeat_n(i as u8, n as usize));
    }
    for c in sizes.iter_mut() {
        if *c > 0 {
            let k = next_sorted[*c as usize];
            next_sorted[*c as usize] += 1;
            *c = new_sizes[k as usize];
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ modèles adaptatifs

/// Modèle de symboles « quasi adaptatif » : fréquences comptées au décodage, codes canoniques
/// régénérés toutes les `update_cycle` occurrences.
#[derive(Clone)]
struct HuffModel {
    total_syms: u32,
    freq: Vec<u16>,
    sizes: Vec<u8>,
    max_cycle: u32,
    update_cycle: u32,
    until_update: u32,
    total_count: u32,
    fast: bool,
    polar: bool,
    // tables de décodage canoniques
    first_code: [u32; MAX_CODE_SIZE + 1],
    count: [u32; MAX_CODE_SIZE + 1],
    offset: [u32; MAX_CODE_SIZE + 1],
    sorted: Vec<u16>,
    max_len: u32,
    table_bits: u32,
    /// symbole | longueur << 16, ou u32::MAX
    lookup: Vec<u32>,
}

impl HuffModel {
    fn new(total_syms: u32, fast: bool, polar: bool) -> Result<Self> {
        // LZHAM_MORE_FREQUENT_TABLE_UPDATING = 1 dans la référence
        let max_cycle = if fast { (total_syms.max(64) + 6) << 5 } else { (total_syms.max(24) + 6) * 12 };
        let mut m = HuffModel {
            total_syms,
            freq: vec![1; total_syms as usize],
            sizes: vec![0; total_syms as usize],
            max_cycle: max_cycle.min(32767),
            update_cycle: 0,
            until_update: 0,
            total_count: 0,
            fast,
            polar,
            first_code: [0; MAX_CODE_SIZE + 1],
            count: [0; MAX_CODE_SIZE + 1],
            offset: [0; MAX_CODE_SIZE + 1],
            sorted: vec![0; total_syms as usize],
            max_len: 0,
            table_bits: 0,
            lookup: Vec::new(),
        };
        m.reset()?;
        Ok(m)
    }

    fn reset(&mut self) -> Result<()> {
        self.freq.iter_mut().for_each(|f| *f = 1);
        self.update_cycle = self.total_syms;
        self.total_count = 0;
        self.until_update = 0;
        self.update()?;
        self.update_cycle = 8;
        self.until_update = 8;
        Ok(())
    }

    fn rescale(&mut self) {
        let mut total = 0;
        for f in &mut self.freq {
            *f = (*f + 1) >> 1;
            total += *f as u32;
        }
        self.total_count = total;
    }

    fn reset_update_rate(&mut self) {
        self.total_count += self.update_cycle - self.until_update;
        if self.total_count > self.total_syms {
            self.rescale();
        }
        self.update_cycle = self.update_cycle.min(8);
        self.until_update = self.update_cycle;
    }

    fn update(&mut self) -> Result<()> {
        self.total_count += self.update_cycle;
        while self.total_count >= 32768 {
            self.rescale();
        }
        let max = if self.polar {
            polar_code_sizes(&self.freq, &mut self.sizes)
        } else {
            huffman_code_sizes(&self.freq, &mut self.sizes)
        };
        if max as usize > MAX_CODE_SIZE {
            limit_code_sizes(&mut self.sizes, MAX_CODE_SIZE)?;
        }
        self.build_tables()?;
        self.update_cycle = if self.fast { 2 * self.update_cycle } else { (5 * self.update_cycle) >> 2 };
        self.update_cycle = self.update_cycle.min(self.max_cycle);
        self.until_update = self.update_cycle;
        Ok(())
    }

    /// Codes canoniques : par longueur croissante, puis par numéro de symbole.
    fn build_tables(&mut self) -> Result<()> {
        let mut count = [0u32; MAX_CODE_SIZE + 1];
        for &s in &self.sizes {
            ensure!((s as usize) <= MAX_CODE_SIZE, "LZHAM : code de {s} bits");
            count[s as usize] += 1;
        }
        count[0] = 0;
        let (mut code, mut used) = (0u32, 0u32);
        self.max_len = 0;
        for len in 1..=MAX_CODE_SIZE {
            self.first_code[len] = code;
            self.offset[len] = used;
            if count[len] > 0 {
                self.max_len = len as u32;
            }
            code += count[len];
            used += count[len];
            code <<= 1;
        }
        ensure!(self.max_len > 0, "LZHAM : table de codes vide");
        self.count = count;
        let mut next = self.offset;
        for (sym, &s) in self.sizes.iter().enumerate() {
            if s > 0 {
                self.sorted[next[s as usize] as usize] = sym as u16;
                next[s as usize] += 1;
            }
        }
        let tb = TABLE_BITS.min(self.max_len);
        self.table_bits = tb;
        self.lookup.clear();
        self.lookup.resize(1 << tb, u32::MAX);
        for len in 1..=tb as usize {
            for c in 0..count[len] {
                let sym = self.sorted[(self.offset[len] + c) as usize] as u32;
                let code = self.first_code[len] + c;
                let fill = tb as usize - len;
                let base = (code as usize) << fill;
                self.lookup[base..base + (1 << fill)].fill(sym | ((len as u32) << 16));
            }
        }
        Ok(())
    }

    #[inline]
    fn decode(&mut self, bits: &mut Bits) -> Result<u32> {
        let k = bits.peek(16);
        let t = self.lookup[(k >> (16 - self.table_bits)) as usize];
        let (sym, len) = if t != u32::MAX {
            (t & 0xFFFF, t >> 16)
        } else {
            let mut found = None;
            for len in self.table_bits + 1..=self.max_len {
                let l = len as usize;
                let code = k >> (16 - len);
                if self.count[l] > 0 && code >= self.first_code[l] && code - self.first_code[l] < self.count[l] {
                    found = Some((self.sorted[(self.offset[l] + code - self.first_code[l]) as usize] as u32, len));
                    break;
                }
            }
            match found {
                Some(f) => f,
                None => bail!("LZHAM : code préfixe invalide"),
            }
        };
        bits.pos += len as usize;
        self.freq[sym as usize] += 1;
        self.until_update -= 1;
        if self.until_update == 0 {
            self.update()?;
        }
        Ok(sym)
    }
}

// ------------------------------------------------------------------ positions LZX

struct Slots {
    base: [u32; LZX_MAX_POSITION_SLOTS],
    extra_bits: [u8; LZX_MAX_POSITION_SLOTS],
    num: u32,
}

impl Slots {
    fn new(dict_size_log2: u32) -> Self {
        let mut extra_bits = [0u8; LZX_MAX_POSITION_SLOTS];
        let mut j = 0u8;
        for i in (0..LZX_MAX_POSITION_SLOTS).step_by(2) {
            extra_bits[i] = j;
            extra_bits[i + 1] = j;
            if i != 0 && j < 25 {
                j += 1;
            }
        }
        let mut base = [0u32; LZX_MAX_POSITION_SLOTS];
        let mut b = 0u64;
        for i in 0..LZX_MAX_POSITION_SLOTS {
            base[i] = b as u32;
            b += 1u64 << extra_bits[i];
        }
        // première classe qui contient la plus grande distance possible
        let largest = (1u64 << dict_size_log2) - 1;
        let num = (0..LZX_MAX_POSITION_SLOTS)
            .find(|&i| largest >= base[i] as u64 && largest < base[i] as u64 + (1u64 << extra_bits[i]))
            .map(|i| i as u32 + 1)
            .unwrap_or(LZX_MAX_POSITION_SLOTS as u32);
        Slots { base, extra_bits, num }
    }
}

// ------------------------------------------------------------------ décodeur

struct Tables {
    lit: Vec<HuffModel>,
    delta_lit: Vec<HuffModel>,
    main: HuffModel,
    rep_len: [HuffModel; 2],
    large_len: [HuffModel; 2],
    dist_lsb: HuffModel,
    is_match: Vec<BitModel>,
    is_rep: [BitModel; NUM_STATES],
    is_rep0: [BitModel; NUM_STATES],
    is_rep0_single: [BitModel; NUM_STATES],
    is_rep1: [BitModel; NUM_STATES],
    is_rep2: [BitModel; NUM_STATES],
}

impl Tables {
    fn new(slots: &Slots, fast: bool, polar: bool) -> Result<Self> {
        let lit = HuffModel::new(256, fast, polar)?;
        let main_syms = LZX_NUM_SPECIAL_LENGTHS + (slots.num - LZX_LOWEST_USABLE_MATCH_SLOT) * 8;
        let rep = HuffModel::new(NUM_HUGE_MATCH_CODES + (MAX_MATCH_LEN - MIN_MATCH_LEN + 1), fast, polar)?;
        let large = HuffModel::new(NUM_HUGE_MATCH_CODES + LZX_NUM_SECONDARY_LENGTHS, fast, polar)?;
        Ok(Tables {
            lit: vec![lit.clone(); 1 << NUM_LIT_PRED_BITS],
            delta_lit: vec![lit; 1 << NUM_DELTA_LIT_PRED_BITS],
            main: HuffModel::new(main_syms, fast, polar)?,
            rep_len: [rep.clone(), rep],
            large_len: [large.clone(), large],
            dist_lsb: HuffModel::new(16, fast, polar)?,
            is_match: vec![BitModel::default(); NUM_STATES << NUM_IS_MATCH_CONTEXT_BITS],
            is_rep: Default::default(),
            is_rep0: Default::default(),
            is_rep0_single: Default::default(),
            is_rep1: Default::default(),
            is_rep2: Default::default(),
        })
    }

    fn reset_update_rates(&mut self) {
        for m in self.lit.iter_mut().chain(self.delta_lit.iter_mut()) {
            m.reset_update_rate();
        }
        for m in self.rep_len.iter_mut().chain(self.large_len.iter_mut()) {
            m.reset_update_rate();
        }
        self.main.reset_update_rate();
        self.dist_lsb.reset_update_rate();
    }

    fn reset_all(&mut self) -> Result<()> {
        // la référence réinitialise le premier modèle de chaque groupe et le recopie
        self.lit[0].reset()?;
        let l0 = self.lit[0].clone();
        self.lit.iter_mut().skip(1).for_each(|m| *m = l0.clone());
        self.delta_lit[0].reset()?;
        let d0 = self.delta_lit[0].clone();
        self.delta_lit.iter_mut().skip(1).for_each(|m| *m = d0.clone());
        self.main.reset()?;
        for m in self.rep_len.iter_mut().chain(self.large_len.iter_mut()) {
            m.reset()?;
        }
        self.dist_lsb.reset()?;
        self.is_match.iter_mut().for_each(|m| *m = BitModel::default());
        for a in [&mut self.is_rep, &mut self.is_rep0, &mut self.is_rep0_single, &mut self.is_rep1, &mut self.is_rep2] {
            a.iter_mut().for_each(|m| *m = BitModel::default());
        }
        Ok(())
    }

    fn flush(&mut self, kind: u32) -> Result<()> {
        match kind {
            1 => self.reset_update_rates(),
            2 => self.reset_all()?,
            _ => {}
        }
        Ok(())
    }
}

/// Longueur « énorme » (au-delà de 257 octets) : préfixe unaire de 0 à 3, puis 8 à 16 bits.
fn huge_len(bits: &mut Bits) -> u32 {
    let mut n = 0;
    while n < 3 && bits.get(1) == 1 {
        n += 1;
    }
    HUGE_MATCH_BASE_LEN[n] + bits.get(HUGE_MATCH_CODE_LEN[n])
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += x as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

/// Décompresse un flux LZHAM alpha dans son enveloppe zlib (2 octets d'en-tête), dont la taille
/// décompressée est connue. La somme Adler-32 finale est vérifiée.
pub fn decompress_zlib(src: &[u8], out_len: usize) -> Result<Vec<u8>> {
    ensure!(src.len() >= 2, "LZHAM : flux trop court");
    let (cmf, flg) = (src[0] as u32, src[1] as u32);
    ensure!(
        ((cmf << 8) | flg) % 31 == 0 && cmf & 15 == ZLIB_METHOD_LZHAM,
        "LZHAM : en-tête zlib invalide ({cmf:02X} {flg:02X})"
    );
    ensure!(flg & 32 == 0, "LZHAM : dictionnaire prédéfini non géré");
    decompress(&src[2..], out_len, (cmf >> 4) + 15)
}

/// Décompresse un flux LZHAM alpha brut. `dict_size_log2` est la taille du dictionnaire utilisée
/// à la compression (elle fixe le nombre de classes de distance).
pub fn decompress(src: &[u8], out_len: usize, dict_size_log2: u32) -> Result<Vec<u8>> {
    ensure!((15..=29).contains(&dict_size_log2), "LZHAM : dictionnaire de 2^{dict_size_log2} octets");
    let slots = Slots::new(dict_size_log2);
    let mut bits = Bits { d: src, pos: 0 };
    let cfg = bits.get(2);
    let mut t = Tables::new(&slots, cfg & 2 != 0, cfg & 1 != 0)?;
    let mut out = vec![0u8; out_len];
    let mut dst = 0usize;

    loop {
        ensure!(!bits.overrun(), "LZHAM : fin du flux inattendue");
        match bits.get(2) {
            0 => {
                // synchronisation
                let kind = bits.get(2);
                t.flush(kind)?;
                bits.align();
                ensure!(bits.get(16) == 0 && bits.get(16) == 0xFFFF, "LZHAM : bloc de synchronisation invalide");
            }
            1 => {
                let mut ar = Arith::start(&mut bits);
                // historique des 4 dernières distances, état de l'analyseur, 2 derniers octets
                let (mut h0, mut h1, mut h2, mut h3) = (1usize, 1usize, 1usize, 1usize);
                let mut state = 0u32;
                let (mut prev, mut prev2) = (0u32, 0u32);
                let kind = bits.get(2);
                t.flush(kind)?;
                loop {
                    let mi = ((prev >> (8 - NUM_IS_MATCH_CONTEXT_BITS)) + (state << NUM_IS_MATCH_CONTEXT_BITS)) as usize;
                    if ar.bit(&mut bits, &mut t.is_match[mi]) == 0 {
                        // littéral
                        ensure!(dst < out_len, "LZHAM : sortie trop petite");
                        let r = if state < NUM_LIT_STATES {
                            let half = NUM_LIT_PRED_BITS / 2;
                            let pred = (prev >> (8 - half)) | ((prev2 >> (8 - half)) << half);
                            t.lit[pred as usize].decode(&mut bits)?
                        } else {
                            // littéral « delta » : xor avec l'octet à la distance de la dernière correspondance
                            let o = dst.wrapping_sub(h0);
                            let rep0 = *out.get(o).unwrap_or(&0) as u32;
                            let rep1 = *out.get(o.wrapping_sub(1)).unwrap_or(&0) as u32;
                            let half = NUM_DELTA_LIT_PRED_BITS / 2;
                            let pred = (rep0 >> (8 - half)) | ((rep1 >> (8 - half)) << half);
                            t.delta_lit[pred as usize].decode(&mut bits)? ^ rep0
                        };
                        out[dst] = r as u8;
                        prev2 = prev;
                        prev = r;
                        state = LITERAL_NEXT_STATE[state as usize] as u32;
                        dst += 1;
                        continue;
                    }
                    let lit_state = state < NUM_LIT_STATES;
                    let mut match_len: u32 = 1;
                    let s = state as usize;
                    if ar.bit(&mut bits, &mut t.is_rep[s]) == 1 {
                        if ar.bit(&mut bits, &mut t.is_rep0[s]) == 1 {
                            if ar.bit(&mut bits, &mut t.is_rep0_single[s]) == 1 {
                                state = if lit_state { 9 } else { 11 };
                            } else {
                                match_len = t.rep_len[!lit_state as usize].decode(&mut bits)? + MIN_MATCH_LEN;
                                if match_len == MAX_MATCH_LEN + 1 {
                                    match_len = huge_len(&mut bits);
                                }
                                state = if lit_state { 8 } else { 11 };
                            }
                        } else {
                            match_len = t.rep_len[!lit_state as usize].decode(&mut bits)? + MIN_MATCH_LEN;
                            if match_len == MAX_MATCH_LEN + 1 {
                                match_len = huge_len(&mut bits);
                            }
                            if ar.bit(&mut bits, &mut t.is_rep1[s]) == 1 {
                                std::mem::swap(&mut h0, &mut h1);
                            } else if ar.bit(&mut bits, &mut t.is_rep2[s]) == 1 {
                                let tmp = h2;
                                h2 = h1;
                                h1 = h0;
                                h0 = tmp;
                            } else {
                                let tmp = h3;
                                h3 = h2;
                                h2 = h1;
                                h1 = h0;
                                h0 = tmp;
                            }
                            state = if lit_state { 8 } else { 11 };
                        }
                    } else {
                        // correspondance complète : longueur (3 bits bas) et classe de distance
                        let sym = t.main.decode(&mut bits)? as i64 - LZX_NUM_SPECIAL_LENGTHS as i64;
                        if sym < 0 {
                            if sym == -2 {
                                break; // fin de bloc
                            }
                            // réinitialisation partielle de l'état
                            h0 = 1;
                            h1 = 1;
                            h2 = 1;
                            h3 = 1;
                            state = 0;
                            continue;
                        }
                        let sym = sym as u32;
                        match_len = (sym & 7) + 2;
                        let slot = ((sym >> 3) + LZX_LOWEST_USABLE_MATCH_SLOT) as usize;
                        ensure!(slot < LZX_MAX_POSITION_SLOTS, "LZHAM : classe de distance invalide");
                        if match_len == 9 {
                            match_len += t.large_len[!lit_state as usize].decode(&mut bits)?;
                            if match_len == MAX_MATCH_LEN + 1 {
                                match_len = huge_len(&mut bits);
                            }
                        }
                        let n_extra = slots.extra_bits[slot] as u32;
                        let extra = if n_extra < 3 {
                            bits.get(n_extra)
                        } else {
                            let mut e = 0;
                            if n_extra > 4 {
                                e = bits.get(n_extra - 4) << 4;
                            }
                            e + t.dist_lsb.decode(&mut bits)?
                        };
                        h3 = h2;
                        h2 = h1;
                        h1 = h0;
                        h0 = (slots.base[slot] + extra) as usize;
                        state = if lit_state { NUM_LIT_STATES } else { NUM_LIT_STATES + 3 };
                    }

                    // copie (octet par octet si la source chevauche la destination)
                    let len = match_len as usize;
                    ensure!(
                        h0 <= dst && h0 > 0 && dst + len <= out_len,
                        "LZHAM : correspondance hors limites (sortie {dst}, distance {h0}, longueur {len})"
                    );
                    let src_ofs = dst - h0;
                    if h0 >= len {
                        out.copy_within(src_ofs..src_ofs + len, dst);
                    } else {
                        for i in 0..len {
                            out[dst + i] = out[src_ofs + i];
                        }
                    }
                    // les deux derniers octets, comme la référence (au début d'un bloc, une copie
                    // d'un seul octet garde le « précédent » du bloc)
                    if len == 1 {
                        prev2 = prev;
                    } else {
                        prev2 = out[dst + len - 2] as u32;
                    }
                    prev = out[dst + len - 1] as u32;
                    dst += len;
                }
                bits.align();
            }
            2 => {
                // bloc brut
                let n = bits.get(24);
                let check = bits.get(8);
                ensure!(check == (n & 0xFF) ^ ((n >> 8) & 0xFF) ^ ((n >> 16) & 0xFF), "LZHAM : bloc brut invalide");
                let n = n as usize + 1;
                bits.align();
                ensure!(dst + n <= out_len, "LZHAM : sortie trop petite");
                let start = bits.pos >> 3;
                ensure!(start + n <= src.len(), "LZHAM : bloc brut tronqué");
                out[dst..dst + n].copy_from_slice(&src[start..start + n]);
                bits.pos += n * 8;
                dst += n;
            }
            _ => break, // fin
        }
    }
    ensure!(dst == out_len, "LZHAM : {dst} octets décodés, {out_len} attendus");
    bits.align();
    let hi = bits.get(16);
    let lo = bits.get(16);
    let expected = (hi << 16) | lo;
    let got = adler32(&out);
    ensure!(got == expected, "LZHAM : somme Adler-32 {got:08X}, {expected:08X} attendue");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_and_adler() {
        // dictionnaire de 1 Mo : 40 classes de distance, la dernière couvre 0xC0000..0xFFFFF
        assert_eq!(Slots::new(20).num, 40);
        assert_eq!(Slots::new(20).base[39], 0xC0000);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }

    /// Les codes sont complets (somme des 2^-longueur = 1).
    #[test]
    fn codes_complete() {
        let freq: Vec<u16> = (0..257).map(|i| (i * 37 % 101 + 1) as u16).collect();
        for polar in [false, true] {
            let mut sizes = vec![0u8; freq.len()];
            let max = if polar { polar_code_sizes(&freq, &mut sizes) } else { huffman_code_sizes(&freq, &mut sizes) };
            if max as usize > MAX_CODE_SIZE {
                limit_code_sizes(&mut sizes, MAX_CODE_SIZE).unwrap();
            }
            let kraft: f64 = sizes.iter().map(|&s| 0.5f64.powi(s as i32)).sum();
            assert!((kraft - 1.0).abs() < 1e-9, "polaire {polar} : {kraft}");
        }
        let mut s2 = vec![0u8; 4];
        huffman_code_sizes(&[100, 1, 1, 1], &mut s2);
        assert_eq!(s2, vec![1, 3, 3, 2]);
    }
}
