//! Raccourcis clavier modifiables : liste des actions, touches par défaut, lecture du clavier
//! et sauvegarde dans la configuration.
//!
//! Deux sortes de touches :
//! - les raccourcis (lecture, gizmo, photo…) sont lus par leur **caractère** : Ctrl+Z reste
//!   Ctrl+Z quelle que soit la disposition du clavier ;
//! - les touches de déplacement sont lues par leur **position** : Z/Q/S/D en AZERTY et W/A/S/D en
//!   QWERTY tombent sous les mêmes doigts.

use eframe::egui::{self, Key};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Action {
    PlayPause,
    PrevFrame,
    NextFrame,
    SetKey,
    MarkIn,
    MarkOut,
    GoToIn,
    GoToOut,
    ClearInOut,
    AddMarker,
    Undo,
    Redo,
    Save,
    DeleteActor,
    Photo,
    GizmoRotate,
    GizmoTranslate,
    FrameSelection,
    GridToSelection,
    ToggleCamera,
    FlyForward,
    FlyBack,
    FlyLeft,
    FlyRight,
    FlyUp,
    FlyDown,
    FlyUpRmb,
    FlyDownRmb,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Group {
    Playback,
    Edit,
    View,
    Fly,
}

impl Group {
    pub const ALL: [Group; 4] = [Group::Playback, Group::Edit, Group::View, Group::Fly];

    pub fn title(self) -> &'static str {
        match self {
            Group::Playback => tr!("▶ Lecture et timeline", "▶ Playback and timeline"),
            Group::Edit => tr!("✏ Édition et fichier", "✏ Editing and file"),
            Group::View => tr!("🎬 Vue et gizmo", "🎬 View and gizmo"),
            Group::Fly => tr!("🕹 Déplacement dans la vue", "🕹 Moving in the viewport"),
        }
    }
}

impl Action {
    pub const ALL: [Action; 28] = [
        Action::PlayPause,
        Action::PrevFrame,
        Action::NextFrame,
        Action::SetKey,
        Action::MarkIn,
        Action::MarkOut,
        Action::GoToIn,
        Action::GoToOut,
        Action::ClearInOut,
        Action::AddMarker,
        Action::Undo,
        Action::Redo,
        Action::Save,
        Action::DeleteActor,
        Action::Photo,
        Action::GizmoRotate,
        Action::GizmoTranslate,
        Action::FrameSelection,
        Action::GridToSelection,
        Action::ToggleCamera,
        Action::FlyForward,
        Action::FlyBack,
        Action::FlyLeft,
        Action::FlyRight,
        Action::FlyUp,
        Action::FlyDown,
        Action::FlyUpRmb,
        Action::FlyDownRmb,
    ];

    fn index(self) -> usize {
        Self::ALL.iter().position(|&a| a == self).unwrap_or(0)
    }

    /// Nom stable, utilisé dans le fichier de configuration.
    pub fn id(self) -> &'static str {
        match self {
            Action::PlayPause => "play_pause",
            Action::PrevFrame => "prev_frame",
            Action::NextFrame => "next_frame",
            Action::SetKey => "set_key",
            Action::MarkIn => "mark_in",
            Action::MarkOut => "mark_out",
            Action::GoToIn => "go_to_in",
            Action::GoToOut => "go_to_out",
            Action::ClearInOut => "clear_in_out",
            Action::AddMarker => "add_marker",
            Action::Undo => "undo",
            Action::Redo => "redo",
            Action::Save => "save",
            Action::DeleteActor => "delete_actor",
            Action::Photo => "photo",
            Action::GizmoRotate => "gizmo_rotate",
            Action::GizmoTranslate => "gizmo_translate",
            Action::FrameSelection => "frame_selection",
            Action::GridToSelection => "grid_to_selection",
            Action::ToggleCamera => "toggle_camera",
            Action::FlyForward => "fly_forward",
            Action::FlyBack => "fly_back",
            Action::FlyLeft => "fly_left",
            Action::FlyRight => "fly_right",
            Action::FlyUp => "fly_up",
            Action::FlyDown => "fly_down",
            Action::FlyUpRmb => "fly_up_rmb",
            Action::FlyDownRmb => "fly_down_rmb",
        }
    }

    pub fn from_id(id: &str) -> Option<Action> {
        Self::ALL.into_iter().find(|a| a.id() == id)
    }

    pub fn label(self) -> &'static str {
        match self {
            Action::PlayPause => tr!("Lecture / pause", "Play / pause"),
            Action::PrevFrame => tr!("Image précédente", "Previous frame"),
            Action::NextFrame => tr!("Image suivante", "Next frame"),
            Action::SetKey => tr!("Poser une clé", "Set a key"),
            Action::MarkIn => tr!("Marquer l'entrée", "Mark in"),
            Action::MarkOut => tr!("Marquer la sortie", "Mark out"),
            Action::GoToIn => tr!("Atteindre l'entrée", "Go to in point"),
            Action::GoToOut => tr!("Atteindre la sortie", "Go to out point"),
            Action::ClearInOut => tr!("Effacer l'entrée et la sortie", "Clear in and out points"),
            Action::AddMarker => tr!("Ajouter un marqueur", "Add a marker"),
            Action::Undo => tr!("Annuler", "Undo"),
            Action::Redo => tr!("Rétablir", "Redo"),
            Action::Save => tr!("Enregistrer le projet", "Save project"),
            Action::DeleteActor => tr!("Supprimer l'acteur sélectionné", "Delete selected actor"),
            Action::Photo => tr!("Photo (caméra de prise)", "Photo (shot camera)"),
            Action::GizmoRotate => tr!("Gizmo de rotation", "Rotation gizmo"),
            Action::GizmoTranslate => tr!("Gizmo de translation", "Translation gizmo"),
            Action::FrameSelection => tr!("Cadrer la sélection", "Frame selection"),
            Action::GridToSelection => tr!("Placer la grille sous le personnage", "Move grid under the character"),
            Action::ToggleCamera => tr!("Caméra de travail / de prise", "Work / shot camera"),
            Action::FlyForward => tr!("Avancer", "Forward"),
            Action::FlyBack => tr!("Reculer", "Back"),
            Action::FlyLeft => tr!("Aller à gauche", "Left"),
            Action::FlyRight => tr!("Aller à droite", "Right"),
            Action::FlyUp => tr!("Monter", "Up"),
            Action::FlyDown => tr!("Descendre", "Down"),
            Action::FlyUpRmb => tr!("Monter (clic droit tenu)", "Up (right button held)"),
            Action::FlyDownRmb => tr!("Descendre (clic droit tenu)", "Down (right button held)"),
        }
    }

    pub fn group(self) -> Group {
        match self {
            Action::PlayPause
            | Action::PrevFrame
            | Action::NextFrame
            | Action::SetKey
            | Action::MarkIn
            | Action::MarkOut
            | Action::GoToIn
            | Action::GoToOut
            | Action::ClearInOut
            | Action::AddMarker => Group::Playback,
            Action::Undo | Action::Redo | Action::Save | Action::DeleteActor | Action::Photo => Group::Edit,
            Action::GizmoRotate
            | Action::GizmoTranslate
            | Action::FrameSelection
            | Action::GridToSelection
            | Action::ToggleCamera => Group::View,
            _ => Group::Fly,
        }
    }

    /// Touche de déplacement : lue par sa position physique, tenue enfoncée, sans modificateur.
    pub fn is_fly(self) -> bool {
        self.group() == Group::Fly
    }

    /// Ne fonctionne qu'en tenant le clic droit (les lettres servent aussi de raccourcis).
    fn needs_rmb(self) -> bool {
        matches!(self, Action::FlyUpRmb | Action::FlyDownRmb)
    }

    fn defaults(self) -> [Option<Binding>; 2] {
        use Binding as B;
        let one = |b: Binding| [Some(b), None];
        match self {
            Action::PlayPause => one(B::key(Key::Space)),
            Action::PrevFrame => one(B::key(Key::ArrowLeft)),
            Action::NextFrame => one(B::key(Key::ArrowRight)),
            Action::SetKey => one(B::key(Key::K)),
            Action::MarkIn => one(B::key(Key::I)),
            Action::MarkOut => one(B::key(Key::O)),
            Action::GoToIn => one(B { shift: true, ..B::key(Key::I) }),
            Action::GoToOut => one(B { shift: true, ..B::key(Key::O) }),
            Action::ClearInOut => one(B { shift: true, ..B::ctrl(Key::X) }),
            Action::AddMarker => one(B::key(Key::M)),
            Action::Undo => one(B::ctrl(Key::Z)),
            Action::Redo => [Some(B::ctrl(Key::Y)), Some(B { shift: true, ..B::ctrl(Key::Z) })],
            Action::Save => one(B::ctrl(Key::S)),
            Action::DeleteActor => one(B::key(Key::Delete)),
            Action::Photo => one(B::key(Key::P)),
            Action::GizmoRotate => one(B::key(Key::E)),
            Action::GizmoTranslate => one(B::key(Key::W)),
            Action::FrameSelection => one(B::key(Key::F)),
            Action::GridToSelection => one(B::key(Key::G)),
            Action::ToggleCamera => one(B::key(Key::C)),
            // positions physiques QWERTY (= Z/S/Q/D en AZERTY)
            Action::FlyForward => one(B::key(Key::W)),
            Action::FlyBack => one(B::key(Key::S)),
            Action::FlyLeft => one(B::key(Key::A)),
            Action::FlyRight => one(B::key(Key::D)),
            Action::FlyUp => one(B::key(Key::PageUp)),
            Action::FlyDown => one(B::key(Key::PageDown)),
            Action::FlyUpRmb => one(B::key(Key::E)),
            Action::FlyDownRmb => one(B::key(Key::Q)),
        }
    }
}

// ------------------------------------------------------------------ touche

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Binding {
    pub key: Key,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl Binding {
    pub const fn key(key: Key) -> Self {
        Self { key, ctrl: false, shift: false, alt: false }
    }

    pub const fn ctrl(key: Key) -> Self {
        Self { key, ctrl: true, shift: false, alt: false }
    }

    pub fn from_mods(key: Key, m: egui::Modifiers) -> Self {
        Self { key, ctrl: m.command, shift: m.shift, alt: m.alt }
    }

    fn matches_mods(&self, m: &egui::Modifiers) -> bool {
        m.command == self.ctrl && m.shift == self.shift && m.alt == self.alt
    }

    /// Forme enregistrée dans la configuration : « Ctrl+Shift+Z ».
    fn to_id(self) -> String {
        let mut s = String::new();
        if self.ctrl {
            s += "Ctrl+";
        }
        if self.shift {
            s += "Shift+";
        }
        if self.alt {
            s += "Alt+";
        }
        s + self.key.name()
    }

    fn parse(s: &str) -> Option<Self> {
        let mut b = Binding::key(Key::Space);
        let mut key = None;
        for part in s.split('+') {
            match part.trim() {
                "Ctrl" => b.ctrl = true,
                "Shift" => b.shift = true,
                "Alt" => b.alt = true,
                "" => {} // la touche « + » elle-même
                k => key = Key::from_name(k),
            }
        }
        if s.ends_with("++") || s == "+" {
            key = Key::from_name("+");
        }
        b.key = key?;
        Some(b)
    }

    /// Texte affiché : « Ctrl+Maj+Z ». Une touche de déplacement porte le nom de la touche qui
    /// se trouve à cette position sur le clavier actuel (Z en AZERTY pour la position du W).
    pub fn label(self, physical: bool) -> String {
        let mut s = String::new();
        if self.ctrl {
            s += "Ctrl+";
        }
        if self.shift {
            s += tr!("Maj+", "Shift+");
        }
        if self.alt {
            s += "Alt+";
        }
        s + &if physical { physical_name(self.key) } else { key_name(self.key).to_string() }
    }
}

/// Nom français d'une touche (les flèches sont écrites en toutes lettres : la police de
/// l'interface n'a pas les caractères ← →).
pub fn key_name(key: Key) -> &'static str {
    match key {
        Key::Space => tr!("Espace", "Space"),
        Key::ArrowLeft => tr!("Flèche gauche", "Left arrow"),
        Key::ArrowRight => tr!("Flèche droite", "Right arrow"),
        Key::ArrowUp => tr!("Flèche haut", "Up arrow"),
        Key::ArrowDown => tr!("Flèche bas", "Down arrow"),
        Key::Delete => tr!("Suppr", "Del"),
        Key::Backspace => tr!("Retour arrière", "Backspace"),
        Key::Enter => tr!("Entrée", "Enter"),
        Key::Insert => tr!("Inser", "Ins"),
        Key::Home => tr!("Début", "Home"),
        Key::End => tr!("Fin", "End"),
        Key::PageUp => tr!("Pg.Préc", "PgUp"),
        Key::PageDown => tr!("Pg.Suiv", "PgDn"),
        Key::Escape => tr!("Échap", "Esc"),
        k => k.name(),
    }
}

/// Nom de la touche située à la position physique `key` (position QWERTY) sur le clavier actuel.
fn physical_name(key: Key) -> String {
    match layout_letter(key) {
        Some(c) => c.to_string(),
        None => key_name(key).to_string(),
    }
}

/// Code de balayage (position) des lettres sur un clavier QWERTY.
fn scancode(key: Key) -> Option<u32> {
    Some(match key {
        Key::Q => 0x10,
        Key::W => 0x11,
        Key::E => 0x12,
        Key::R => 0x13,
        Key::T => 0x14,
        Key::Y => 0x15,
        Key::U => 0x16,
        Key::I => 0x17,
        Key::O => 0x18,
        Key::P => 0x19,
        Key::A => 0x1E,
        Key::S => 0x1F,
        Key::D => 0x20,
        Key::F => 0x21,
        Key::G => 0x22,
        Key::H => 0x23,
        Key::J => 0x24,
        Key::K => 0x25,
        Key::L => 0x26,
        Key::Z => 0x2C,
        Key::X => 0x2D,
        Key::C => 0x2E,
        Key::V => 0x2F,
        Key::B => 0x30,
        Key::N => 0x31,
        Key::M => 0x32,
        _ => return None,
    })
}

/// Lettre imprimée sur la touche à cette position, selon la disposition active de Windows.
#[cfg(windows)]
fn layout_letter(key: Key) -> Option<char> {
    #[link(name = "user32")]
    extern "system" {
        fn MapVirtualKeyW(code: u32, map_type: u32) -> u32;
    }
    const MAPVK_VSC_TO_VK: u32 = 1;
    let sc = scancode(key)?;
    // SAFETY : fonction Win32 sans pointeur, appelable depuis n'importe quel fil
    let vk = unsafe { MapVirtualKeyW(sc, MAPVK_VSC_TO_VK) };
    char::from_u32(vk).filter(|c| c.is_ascii_uppercase())
}

#[cfg(not(windows))]
fn layout_letter(key: Key) -> Option<char> {
    scancode(key)?;
    key.name().chars().next()
}

// ------------------------------------------------------------------ table

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(from = "BTreeMap<String, Vec<String>>", into = "BTreeMap<String, Vec<String>>")]
pub struct Keymap {
    slots: Vec<[Option<Binding>; 2]>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self { slots: Action::ALL.iter().map(|a| a.defaults()).collect() }
    }
}

impl From<BTreeMap<String, Vec<String>>> for Keymap {
    fn from(map: BTreeMap<String, Vec<String>>) -> Self {
        let mut km = Keymap::default();
        for (id, keys) in map {
            let Some(a) = Action::from_id(&id) else { continue };
            let mut slot = [None, None];
            for (i, k) in keys.iter().take(2).enumerate() {
                slot[i] = Binding::parse(k);
            }
            km.slots[a.index()] = slot;
        }
        km
    }
}

impl From<Keymap> for BTreeMap<String, Vec<String>> {
    fn from(km: Keymap) -> Self {
        Action::ALL
            .iter()
            .map(|&a| {
                let keys = km.get(a).iter().map(|b| b.map(Binding::to_id).unwrap_or_default()).collect();
                (a.id().to_string(), keys)
            })
            .collect()
    }
}

impl Keymap {
    pub fn get(&self, a: Action) -> [Option<Binding>; 2] {
        self.slots[a.index()]
    }

    pub fn is_default(&self, a: Action) -> bool {
        self.get(a) == a.defaults()
    }

    pub fn reset(&mut self, a: Action) {
        self.slots[a.index()] = a.defaults();
    }

    /// Pose une touche. Si une autre action de même sorte l'utilisait déjà, elle la perd :
    /// renvoie cette action.
    pub fn set(&mut self, a: Action, slot: usize, b: Option<Binding>) -> Option<Action> {
        let mut stolen = None;
        if let Some(b) = b {
            for other in Action::ALL {
                if other == a || other.is_fly() != a.is_fly() {
                    continue;
                }
                for s in &mut self.slots[other.index()] {
                    if *s == Some(b) {
                        *s = None;
                        stolen = Some(other);
                    }
                }
            }
            // pas de doublon dans la même action
            let own = &mut self.slots[a.index()];
            if own[1 - slot] == Some(b) {
                own[1 - slot] = None;
            }
        }
        self.slots[a.index()][slot] = b;
        stolen
    }

    /// Le raccourci vient d'être pressé (avec exactement ses modificateurs).
    pub fn pressed(&self, i: &egui::InputState, a: Action) -> bool {
        self.get(a).iter().flatten().any(|b| b.matches_mods(&i.modifiers) && i.key_pressed(b.key))
    }

    /// Une touche de déplacement est tenue (`keys` : positions physiques enfoncées).
    pub fn held(&self, keys: &HashSet<Key>, a: Action) -> bool {
        self.get(a).iter().flatten().any(|b| keys.contains(&b.key))
    }

    /// Texte d'une touche, ou « — » si elle est vide.
    pub fn slot_label(&self, a: Action, slot: usize) -> Option<String> {
        self.get(a)[slot].map(|b| b.label(a.is_fly()))
    }

    /// Toutes les touches de l'action : « Ctrl+Y / Ctrl+Maj+Z ».
    pub fn label(&self, a: Action) -> String {
        let v: Vec<String> = (0..2).filter_map(|s| self.slot_label(a, s)).collect();
        if v.is_empty() {
            tr!("aucune touche", "no key").into()
        } else {
            v.join(" / ")
        }
    }

    /// Autre action qui réagit à la même touche (même texte affiché, par exemple un raccourci W
    /// et la touche Avancer en QWERTY). Les touches « clic droit tenu » ne comptent pas : les
    /// raccourcis sont coupés pendant le vol.
    pub fn clash(&self, a: Action, slot: usize) -> Option<Action> {
        if a.needs_rmb() {
            return None;
        }
        let me = self.slot_label(a, slot)?;
        Action::ALL.into_iter().find(|&o| {
            o != a && !o.needs_rmb() && (0..2).any(|s| self.slot_label(o, s).as_deref() == Some(me.as_str()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let mut km = Keymap::default();
        km.set(Action::Photo, 1, Some(Binding { shift: true, ..Binding::ctrl(Key::P) }));
        km.set(Action::SetKey, 0, None);
        let json = serde_json::to_string(&km).unwrap();
        let back: Keymap = serde_json::from_str(&json).unwrap();
        assert_eq!(km, back);
    }

    #[test]
    fn missing_actions_keep_defaults() {
        let km: Keymap = serde_json::from_str(r#"{"photo":["O",""]}"#).unwrap();
        assert_eq!(km.get(Action::Photo), [Some(Binding::key(Key::O)), None]);
        assert!(km.is_default(Action::PlayPause));
    }

    #[test]
    fn set_steals_from_other_action() {
        let mut km = Keymap::default();
        let stolen = km.set(Action::Photo, 0, Some(Binding::key(Key::K)));
        assert_eq!(stolen, Some(Action::SetKey));
        assert_eq!(km.get(Action::SetKey), [None, None]);
        // une touche de déplacement ne vole pas un raccourci
        assert_eq!(km.set(Action::FlyForward, 0, Some(Binding::key(Key::P))), None);
    }
}
