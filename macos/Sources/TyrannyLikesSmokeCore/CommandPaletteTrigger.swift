/// Decides whether a key press opens the `:` command palette.
///
/// `:` is not a menu key equivalent, because menu keys beat text fields in this app.
/// A key monitor asks this instead, so `:` still types into any field that has focus.
public enum CommandPaletteTrigger {
    /// True for a plain `:` in the palette's window, with no sheet up and no text being edited.
    /// Shift is allowed, since most layouts need it for `:`. ⌘, ⌃ and ⌥ are not.
    public static func shouldOpen(
        characters: String?, hasCommandModifiers: Bool, isPaletteWindow: Bool, hasSheet: Bool, isEditingText: Bool
    ) -> Bool {
        characters == ":" && !hasCommandModifiers && isPaletteWindow && !hasSheet && !isEditingText
    }
}
