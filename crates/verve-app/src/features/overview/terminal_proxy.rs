//! Shell-specific terminal proxy environment variable command formatting.

/// Supported desktop terminal shells for copying proxy environment commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TerminalShell {
    /// Windows PowerShell / PowerShell 7 (`$env:...`).
    PowerShell,
    /// GNU Bash (`export ...`).
    Bash,
    /// Z shell (`export ...`).
    Zsh,
    /// Friendly Interactive Shell (`set -x ...`).
    Fish,
    /// Windows Command Prompt (`set ... && ...`).
    Cmd,
}

impl TerminalShell {
    /// All supported terminal shells in display order.
    pub const ALL: [Self; 5] = [
        Self::PowerShell,
        Self::Bash,
        Self::Zsh,
        Self::Fish,
        Self::Cmd,
    ];

    /// Returns the platform-appropriate default shell (`PowerShell` on Windows, `Bash` on Unix).
    #[must_use]
    pub const fn default_for_platform() -> Self {
        #[cfg(windows)]
        {
            Self::PowerShell
        }
        #[cfg(not(windows))]
        {
            Self::Bash
        }
    }

    /// Stable element/selector identifier string for this shell.
    #[must_use]
    pub const fn id_str(self) -> &'static str {
        match self {
            Self::PowerShell => "powershell",
            Self::Bash => "bash",
            Self::Zsh => "zsh",
            Self::Fish => "fish",
            Self::Cmd => "cmd",
        }
    }

    /// Human-readable display label for this shell.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::PowerShell => "PowerShell",
            Self::Bash => "Bash",
            Self::Zsh => "Zsh",
            Self::Fish => "Fish",
            Self::Cmd => "CMD",
        }
    }
}

/// Formats a one-line shell command exporting `http_proxy`, `https_proxy`, and `all_proxy`
/// pointing to `host:port`.
#[must_use]
pub fn format_terminal_proxy_command(shell: TerminalShell, host: &str, port: u16) -> String {
    match shell {
        TerminalShell::PowerShell => format!(
            "$env:http_proxy=\"http://{host}:{port}\"; $env:https_proxy=\"http://{host}:{port}\"; $env:all_proxy=\"socks5://{host}:{port}\""
        ),
        TerminalShell::Bash | TerminalShell::Zsh => format!(
            "export http_proxy=http://{host}:{port} https_proxy=http://{host}:{port} all_proxy=socks5://{host}:{port}"
        ),
        TerminalShell::Fish => format!(
            "set -x http_proxy http://{host}:{port}; set -x https_proxy http://{host}:{port}; set -x all_proxy socks5://{host}:{port}"
        ),
        TerminalShell::Cmd => format!(
            "set http_proxy=http://{host}:{port} && set https_proxy=http://{host}:{port} && set all_proxy=socks5://{host}:{port}"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_all_five_shells_with_loopback_host_and_port() {
        assert_eq!(
            format_terminal_proxy_command(TerminalShell::PowerShell, "127.0.0.1", 7897),
            "$env:http_proxy=\"http://127.0.0.1:7897\"; $env:https_proxy=\"http://127.0.0.1:7897\"; $env:all_proxy=\"socks5://127.0.0.1:7897\""
        );
        assert_eq!(
            format_terminal_proxy_command(TerminalShell::Bash, "127.0.0.1", 7897),
            "export http_proxy=http://127.0.0.1:7897 https_proxy=http://127.0.0.1:7897 all_proxy=socks5://127.0.0.1:7897"
        );
        assert_eq!(
            format_terminal_proxy_command(TerminalShell::Zsh, "127.0.0.1", 7897),
            "export http_proxy=http://127.0.0.1:7897 https_proxy=http://127.0.0.1:7897 all_proxy=socks5://127.0.0.1:7897"
        );
        assert_eq!(
            format_terminal_proxy_command(TerminalShell::Fish, "127.0.0.1", 7897),
            "set -x http_proxy http://127.0.0.1:7897; set -x https_proxy http://127.0.0.1:7897; set -x all_proxy socks5://127.0.0.1:7897"
        );
        assert_eq!(
            format_terminal_proxy_command(TerminalShell::Cmd, "127.0.0.1", 7897),
            "set http_proxy=http://127.0.0.1:7897 && set https_proxy=http://127.0.0.1:7897 && set all_proxy=socks5://127.0.0.1:7897"
        );
    }

    #[test]
    fn formats_custom_host_and_port_and_platform_default() {
        assert_eq!(
            format_terminal_proxy_command(TerminalShell::Bash, "192.168.1.10", 10808),
            "export http_proxy=http://192.168.1.10:10808 https_proxy=http://192.168.1.10:10808 all_proxy=socks5://192.168.1.10:10808"
        );
        assert_eq!(TerminalShell::ALL.len(), 5);

        #[cfg(windows)]
        assert_eq!(
            TerminalShell::default_for_platform(),
            TerminalShell::PowerShell
        );

        #[cfg(not(windows))]
        assert_eq!(TerminalShell::default_for_platform(), TerminalShell::Bash);
    }
}
