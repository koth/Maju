//! Windows spawn details the provider and its installer share.
//!
//! Both start console-subsystem programs — `node` for the provider, `npm`/`npx`
//! for the install — from a GUI application, and on Windows that opens a visible
//! console window unless the spawn says otherwise. Neither process has any use
//! for one: the provider's MCP conversation runs over stdio pipes and the
//! installer's output is captured. The window is pure noise, and it appears at
//! exactly the wrong moment — the instant the browser tools connect — then stays
//! until the provider exits.
//!
//! `CREATE_NO_WINDOW` is the flag the rest of the app already uses for this
//! (`git-service`, the agent CLIs, the dsh host). What it cannot fix on its own
//! is a shim chain: a `.cmd` wrapper relaunching a console child has no parent
//! console to inherit, so Windows allocates a fresh *visible* one — the "window
//! that flashes by" the dsh host documents in `dsh-bridge::process`. The provider
//! is already launched as `<node> <cli.js>`, straight at the interpreter with no
//! shim in between, so the flag is sufficient there.

/// Windows `CREATE_NO_WINDOW`: spawn without a visible console window.
#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Spawn `command` without a visible console window on Windows.
///
/// A no-op elsewhere — non-Windows processes have no console window to hide.
///
/// `creation_flags` is tokio's own method on Windows, so no trait import is
/// needed here.
pub fn hide_console(command: &mut tokio::process::Command) {
    #[cfg(windows)]
    {
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = command;
    }
}
