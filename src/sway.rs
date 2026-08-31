//! sway is the source of truth for the window list (`swaymsg -t get_tree`); the
//! Wayland side only supplies pixels. The join between the two is
//! `foreign_toplevel_identifier`, which sway reports per view.
//!
//! Acting on the choice is deliberately not here: wl-pick reports what was picked
//! and the caller decides what that means.

use swayipc::{Connection, Node, NodeType};

use crate::target::Target;

/// Every view in the tree, in tree order (the same traversal the jq filter did,
/// so the grid keeps the ordering the muscle memory expects).
pub fn windows(conn: &mut Connection) -> Result<Vec<Target>, swayipc::Error> {
    let mut out = Vec::new();
    collect(&conn.get_tree()?, &mut out);
    Ok(out)
}

fn collect(node: &Node, out: &mut Vec<Target>) {
    let is_con = matches!(node.node_type, NodeType::Con | NodeType::FloatingCon);
    let class = node
        .window_properties
        .as_ref()
        .and_then(|p| p.class.clone());
    if is_con && (node.app_id.is_some() || class.is_some()) {
        // A view with no identifier can't be captured, but it still belongs in
        // the list: it gets a tile with no thumbnail.
        out.push(Target::window(
            node.id,
            node.foreign_toplevel_identifier.clone().unwrap_or_default(),
            node.app_id.clone().or(class).unwrap_or_default(),
            node.name.clone().unwrap_or_default(),
        ));
    }
    for child in node.nodes.iter().chain(node.floating_nodes.iter()) {
        collect(child, out);
    }
}

/// One active display: what the overlay needs to size itself against.
///
/// The overlay maps on the focused display, so percentages and the buffer scale
/// are resolved against *that* one — on a mixed-DPI, mixed-size setup the
/// numbers differ per monitor, and taking the largest of everything would be
/// wrong on all but one.
#[derive(Clone, Debug)]
pub struct Display {
    pub name: String,
    /// Logical size, which is what layer-shell and pointer events speak in.
    pub width: i32,
    pub height: i32,
    /// Integer scale to render at: a buffer can be downscaled, not invented.
    pub scale: i32,
    pub focused: bool,
}

pub fn displays(conn: &mut Connection) -> Result<Vec<Display>, swayipc::Error> {
    Ok(conn
        .get_outputs()?
        .into_iter()
        .filter(|o| o.active)
        .map(|o| Display {
            name: o.name,
            width: o.rect.width,
            height: o.rect.height,
            scale: (o.scale.unwrap_or(1.0).ceil() as i32).max(1),
            focused: o.focused,
        })
        .collect())
}

/// The display the overlay will appear on: the focused one, or any active one if
/// sway reports none focused.
pub fn focused(displays: &[Display]) -> Option<&Display> {
    displays
        .iter()
        .find(|d| d.focused)
        .or_else(|| displays.first())
}
