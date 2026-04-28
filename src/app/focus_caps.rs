/// Summary of which keymap actions the currently focused row supports.
///
/// `Keymap` reads this value to decide whether to dim a key in-place
/// (disabled) or render it at full brightness (enabled).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FocusCaps {
    pub can_expand: bool,
    pub can_edit: bool,
    pub can_back: bool,
    pub can_input: bool,
}
