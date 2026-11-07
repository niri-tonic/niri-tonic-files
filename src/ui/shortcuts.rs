use gtk::gdk;

/// Application commands use exactly Control. Lock/NumLock do not affect them.
pub(super) fn command(key: gdk::Key, modifiers: gdk::ModifierType) -> Option<&'static str> {
    let relevant = gdk::ModifierType::CONTROL_MASK
        | gdk::ModifierType::SHIFT_MASK
        | gdk::ModifierType::ALT_MASK
        | gdk::ModifierType::SUPER_MASK
        | gdk::ModifierType::META_MASK
        | gdk::ModifierType::HYPER_MASK;
    if modifiers & relevant != gdk::ModifierType::CONTROL_MASK {
        return None;
    }
    match key.to_lower() {
        gdk::Key::a => Some("select-all"),
        gdk::Key::c => Some("copy"),
        gdk::Key::v => Some("paste"),
        gdk::Key::d => Some("delete"),
        gdk::Key::f => Some("favorite"),
        gdk::Key::t => Some("toggle-column"),
        gdk::Key::r => Some("refresh"),
        gdk::Key::Escape => Some("quit"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gdk::ModifierType as M;

    #[test]
    fn delete_requires_the_new_exact_chord() {
        assert_eq!(command(gdk::Key::d, M::CONTROL_MASK), Some("delete"));
        assert_eq!(
            command(gdk::Key::D, M::CONTROL_MASK | M::LOCK_MASK),
            Some("delete")
        );
        for modifiers in [
            M::empty(),
            M::SHIFT_MASK,
            M::CONTROL_MASK | M::SHIFT_MASK,
            M::CONTROL_MASK | M::ALT_MASK,
        ] {
            assert_eq!(command(gdk::Key::d, modifiers), None);
        }
        for modifiers in [M::empty(), M::SHIFT_MASK, M::CONTROL_MASK] {
            assert_eq!(command(gdk::Key::Delete, modifiers), None);
        }
    }

    #[test]
    fn legacy_chords_are_disabled_and_new_navigation_is_explicit() {
        for key in [
            gdk::Key::l,
            gdk::Key::q,
            gdk::Key::w,
            gdk::Key::x,
            gdk::Key::h,
            gdk::Key::n,
            gdk::Key::e,
        ] {
            assert_eq!(command(key, M::CONTROL_MASK), None);
            assert_eq!(command(key, M::CONTROL_MASK | M::SHIFT_MASK), None);
        }
        for key in [gdk::Key::Left, gdk::Key::Right, gdk::Key::Up] {
            assert_eq!(command(key, M::ALT_MASK), None);
        }
        for key in [
            gdk::Key::F1,
            gdk::Key::F2,
            gdk::Key::F5,
            gdk::Key::Menu,
            gdk::Key::BackSpace,
            gdk::Key::Escape,
        ] {
            assert_eq!(command(key, M::empty()), None);
        }
        assert_eq!(command(gdk::Key::F10, M::SHIFT_MASK), None);
        assert_eq!(command(gdk::Key::f, M::CONTROL_MASK), Some("favorite"));
        assert_eq!(command(gdk::Key::t, M::CONTROL_MASK), Some("toggle-column"));
        assert_eq!(command(gdk::Key::r, M::CONTROL_MASK), Some("refresh"));
        assert_eq!(command(gdk::Key::r, M::CONTROL_MASK | M::SHIFT_MASK), None);
        assert_eq!(command(gdk::Key::Escape, M::CONTROL_MASK), Some("quit"));
    }
}
