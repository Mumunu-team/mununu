//! Shared probes for the adapter tests that run a real tool.

/// Whether the host can lift SystemVerilog: a `yosys` on the PATH **with** `read_slang`
/// (built in from 0.67; the pinned `mununu-sva` image has it) or a yosys-slang plugin named
/// by `MUNUNU_YOSYS_SLANG_PLUGIN`. A yosys without it (a brew 0.59) skips the lift tests
/// here instead of failing them — since the slang-only lift (N1, 2026-11-04) `yosys -V`
/// alone is not the question.
pub(crate) fn slang_lift_available() -> bool {
    let mut cmd = std::process::Command::new("yosys");
    if let Some(plugin) = std::env::var("MUNUNU_YOSYS_SLANG_PLUGIN")
        .ok()
        .filter(|p| !p.trim().is_empty())
    {
        cmd.arg("-m").arg(plugin);
    }
    cmd.args(["-p", "help read_slang"])
        .output()
        .map(|o| {
            // `help <unknown>` exits 0 and prints "No such command or cell type" instead.
            o.status.success() && !String::from_utf8_lossy(&o.stdout).contains("No such command")
        })
        .unwrap_or(false)
}
