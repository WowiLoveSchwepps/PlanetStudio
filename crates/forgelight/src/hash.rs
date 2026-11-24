//! Fonctions de hachage utilisées par le moteur Forgelight.

/// CRC-64 "Jones" (réfléchi, init = !0, xorout = !0), appliqué au nom
/// de fichier en MAJUSCULES. Sert d'identifiant des assets dans les .pack2.
pub fn crc64_jones(data: &[u8]) -> u64 {
    static TABLE: std::sync::OnceLock<[u64; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        // polynôme 0xAD93D23594C935A9 réfléchi
        let poly: u64 = 0xAD93D23594C935A9u64.reverse_bits();
        let mut t = [0u64; 256];
        for (i, e) in t.iter_mut().enumerate() {
            let mut c = i as u64;
            for _ in 0..8 {
                c = if c & 1 != 0 { (c >> 1) ^ poly } else { c >> 1 };
            }
            *e = c;
        }
        t
    });
    let mut crc = !0u64;
    for &b in data {
        crc = table[((crc ^ b as u64) & 0xFF) as usize] ^ (crc >> 8);
    }
    !crc
}

/// Hash d'un nom d'asset tel que stocké dans les .pack2.
pub fn name_hash(name: &str) -> u64 {
    crc64_jones(name.to_ascii_uppercase().as_bytes())
}

/// Jenkins "one-at-a-time" en arithmétique *signée* (décalages arithmétiques),
/// comme le fait le moteur. Utilisé pour les définitions de matériaux (nom tel quel),
/// les textures et les os (nom en MAJUSCULES).
pub fn jenkins(data: &[u8]) -> u32 {
    let mut h: i32 = 0;
    for &c in data {
        h = h.wrapping_add(c as i32);
        h = h.wrapping_add(h.wrapping_shl(10));
        h ^= h >> 6;
    }
    h = h.wrapping_add(h.wrapping_shl(3));
    h ^= h >> 11;
    h = h.wrapping_add(h.wrapping_shl(15));
    h as u32
}

pub fn jenkins_upper(s: &str) -> u32 {
    jenkins(s.to_ascii_uppercase().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn known_hashes() {
        assert_eq!(jenkins(b"Character"), 0x833509aa);
        assert_eq!(jenkins_upper("Armor_TR_Shared_All_DS.dds"), 0xe0f49baa);
        assert_eq!(jenkins_upper("WORLDROOT"), 0xa2897232);
    }
}
