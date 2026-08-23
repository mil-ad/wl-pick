//! sway stays the source of truth for the window list and for focusing, exactly
//! as the shell script this replaces did (`swaymsg -t get_tree` + `[con_id=N]
//! focus`). The Wayland side only supplies pixels; the join between the two is
//! `foreign_toplevel_identifier`, which sway reports per view.

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

/// The largest integer scale in use, which is what the overlay renders at.
pub fn scale(conn: &mut Connection) -> Result<i32, swayipc::Error> {
    Ok(conn
        .get_outputs()?
        .iter()
        .filter(|o| o.active)
        .map(|o| o.scale.unwrap_or(1.0).ceil() as i32)
        .max()
        .unwrap_or(1)
        .max(1))
}

pub fn focus(conn: &mut Connection, target: &Target) -> Result<(), swayipc::Error> {
    let cmd = match target.con_id {
        Some(con_id) => format!("[con_id={con_id}] focus"),
        // Picking a display means going to it.
        None => format!("focus output {}", target.id),
    };
    for res in conn.run_command(cmd)? {
        res?;
    }
    Ok(())
}
