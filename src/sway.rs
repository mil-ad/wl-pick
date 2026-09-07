//! sway is the source of truth for the window list (`swaymsg -t get_tree`); the
//! Wayland side only supplies pixels. The join between the two is
//! `foreign_toplevel_identifier`, which sway reports per view.
//!
//! Acting on the choice is deliberately not here: wl-pick reports what was picked
//! and the caller decides what that means.

use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use swayipc::{Connection, Node, NodeType};

use crate::target::Target;

/// Open the IPC connection, recovering when the environment lies about where
/// the socket is.
///
/// swayipc takes the path from `I3SOCK` or `SWAYSOCK` and only falls back to
/// asking sway directly when *neither is set* -- a variable that is set but
/// stale is used as-is, and fails. That happens whenever something in a
/// shell's ancestry outlived the sway that started it: one long-running daemon
/// is enough, and every shell it spawns inherits a path to a socket that no
/// longer exists. Since the running compositor is the one we want either way,
/// go and find its socket instead of failing.
pub fn connect() -> Result<Connection, String> {
    // The environment still wins when it points at something real. Going
    // through swayipc's own lookup instead would spawn `sway
    // --get-socketpath` whenever the variables are unset, which prints a
    // complaint of its own before we can say anything useful.
    if let Some(conn) = env_socket().and_then(|p| UnixStream::connect(p).ok()) {
        return Ok(Connection::from(conn));
    }
    let live = live_sockets();
    let [path] = live.as_slice() else {
        return Err(if live.is_empty() {
            "cannot reach sway; wl-pick reads the window list from its IPC \
             socket, and no running sway has one"
                .to_string()
        } else {
            // Several live compositors, so any choice would be a guess: a
            // nested sway is a real thing to be running.
            format!(
                "several sway sockets to choose from ({}); set SWAYSOCK to the one you mean",
                live.iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        });
    };
    UnixStream::connect(path)
        .map(Connection::from)
        .map_err(|e| format!("cannot reach sway on {} ({e})", path.display()))
}

/// The pid out of a `sway-ipc.<uid>.<pid>.sock` name, and nothing else.
fn socket_pid(name: &str) -> Option<&str> {
    name.strip_prefix("sway-ipc.")?
        .strip_suffix(".sock")?
        .rsplit('.')
        .next()
        .filter(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
}

/// The socket the environment names, if it is actually there. sway's own
/// variable comes second because swayipc reads them in this order.
fn env_socket() -> Option<PathBuf> {
    ["I3SOCK", "SWAYSOCK"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .find(|path| path.exists())
}

/// Sockets in the runtime directory whose sway is still running. They are named
/// `sway-ipc.<uid>.<pid>.sock`, so the pid says which are worth trying -- and
/// pids get reused, so it has to actually be a sway.
fn live_sockets() -> Vec<PathBuf> {
    let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|path| {
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                return false;
            };
            socket_pid(name)
                .and_then(|pid| std::fs::read_to_string(format!("/proc/{pid}/comm")).ok())
                .is_some_and(|comm| comm.trim() == "sway")
        })
        .collect();
    // Stable order, so the message about several of them does not shuffle.
    found.sort();
    found
}

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

#[cfg(test)]
mod tests {
    use super::socket_pid;

    #[test]
    fn a_socket_name_gives_up_its_pid() {
        assert_eq!(socket_pid("sway-ipc.1000.573773.sock"), Some("573773"));
        // Anything that is not a live sway's socket must not be tried: the
        // runtime directory is full of other people's sockets.
        assert_eq!(socket_pid("wayland-1"), None);
        assert_eq!(socket_pid("sway-ipc.1000.573773.sock.bak"), None);
        assert_eq!(socket_pid("i3-ipc.1000.5.sock"), None);
        assert_eq!(socket_pid("sway-ipc.1000..sock"), None);
        assert_eq!(socket_pid("sway-ipc.1000.notapid.sock"), None);
    }
}
