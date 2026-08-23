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

/// The active displays, and the scale the overlay should render at: the largest
/// in use, rounded up, since a buffer can be downscaled but not invented.
pub struct Displays {
    pub(crate) names: Vec<String>,
    pub(crate) scale: i32,
}

pub fn displays(conn: &mut Connection) -> Result<Displays, swayipc::Error> {
    let active: Vec<_> = conn
        .get_outputs()?
        .into_iter()
        .filter(|o| o.active)
        .collect();
    Ok(Displays {
        scale: active
            .iter()
            .map(|o| o.scale.unwrap_or(1.0).ceil() as i32)
            .max()
            .unwrap_or(1)
            .max(1),
        names: active.into_iter().map(|o| o.name).collect(),
    })
}
