use egui::{Context, Key};

#[derive(Clone, Copy)]
pub struct ShortcutBinding<T: Copy> {
    pub key: Key,
    pub action: T,
}

pub fn alt_pressed(ctx: &Context) -> bool {
    ctx.input(|i| i.modifiers.alt)
}

pub fn handle_alt_shortcuts<T: Copy>(ctx: &Context, bindings: &[ShortcutBinding<T>]) -> Option<T> {
    if !alt_pressed(ctx) {
        return None;
    }
    for binding in bindings {
        if ctx.input(|i| i.key_pressed(binding.key)) {
            return Some(binding.action);
        }
    }
    None
}
