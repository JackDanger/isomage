//! Registry of reference tools used across isomage's round-trip tests.
//!
//! Every entry uses the [`Tool::with_aliases`] constructor — the
//! primary name is the modern canonical binary, aliases are
//! historical or platform-specific alternatives that ship the same
//! capability.
//!
//! When a new format lands in `src/formats/`, the corresponding
//! reference tools get added here. The alias chains are deliberately
//! conservative: only binaries with compatible CLIs go in the same
//! `Tool`. If a tool has a meaningfully different CLI, give it its
//! own `Tool` and let the test author switch on
//! [`Tool::resolve().name`] to dispatch.
//!
//! Two-years-of-OSS-development note: each tool also lists what it
//! gets installed via on Debian/Ubuntu and macOS Homebrew, so the
//! CI YAML and this file co-evolve.

use crate::common::tool::Tool;

/// Curated, alphabetically-sorted tool registry. `pub`-exported via
/// `common::tools` (no extra namespace) so tests read tidily:
///
/// ```text
/// use common::tools;
/// let resolved = Skip::if_missing(&tools::QEMU_IMG)?;
/// ```
pub mod tools {
    use super::Tool;

    // --- Partition tables ----------------------------------------

    /// MBR partition-table editor. Linux: `util-linux`. macOS: not
    /// available (use `parted` or `fdisk` alternatives).
    pub const SFDISK: Tool = Tool::new("sfdisk");

    /// GPT partition-table editor. Linux: `gdisk`. macOS: `brew install gptfdisk`.
    pub const SGDISK: Tool = Tool::new("sgdisk");

    /// fdisk — read-only inspector we use for cross-validation.
    /// Linux: `util-linux`. macOS: BSD `fdisk` is incompatible — alias only.
    pub const FDISK: Tool = Tool::new("fdisk");

    /// parted — alternative for both MBR and GPT.
    pub const PARTED: Tool = Tool::new("parted");

    // --- ISO 9660 / UDF ------------------------------------------

    /// ISO 9660 author. Linux: `xorriso` (preferred, modern) or
    /// `genisoimage` (legacy). macOS: `brew install xorriso`.
    /// The `xorriso -as mkisofs` shim makes all three CLI-compatible
    /// for the args we use.
    pub const MKISOFS: Tool = Tool::with_aliases("mkisofs", &["xorriso", "genisoimage"]);

    /// UDF authoring. Linux: `udftools` package. macOS: not packaged.
    pub const MKUDFFS: Tool = Tool::new("mkudffs");

    /// macOS native ISO/HFS+ tooling. Always present on macOS;
    /// never on Linux. Tests that target this skip on Linux runners.
    pub const HDIUTIL: Tool = Tool::new("hdiutil");

    // --- Virtual disk containers ---------------------------------

    /// VHD / VHDX / VMDK / QCOW2 author + inspector.
    /// Linux: `qemu-utils`. macOS: `brew install qemu`.
    pub const QEMU_IMG: Tool = Tool::new("qemu-img");

    // --- FAT family ----------------------------------------------

    /// FAT12/16/32 author. Linux: `dosfstools`. macOS: `brew install dosfstools`.
    /// Alias chain covers the historical names some distros still ship.
    pub const MKFS_VFAT: Tool = Tool::with_aliases("mkfs.vfat", &["mkfs.fat", "mkfs.msdos"]);

    /// FAT consistency checker; used to validate our writer output.
    pub const FSCK_VFAT: Tool = Tool::with_aliases("fsck.vfat", &["fsck.fat", "dosfsck"]);

    /// FAT directory inspection — read-only, used to cross-check tree shape.
    pub const MDIR: Tool = Tool::new("mdir");

    // --- exFAT ---------------------------------------------------

    /// exFAT author. Linux: `exfatprogs` (modern) or `exfat-utils`
    /// (legacy). macOS: `brew install exfatprogs`.
    pub const MKFS_EXFAT: Tool = Tool::with_aliases("mkfs.exfat", &["mkexfatfs"]);

    /// exFAT consistency checker.
    pub const FSCK_EXFAT: Tool = Tool::with_aliases("fsck.exfat", &["exfatfsck"]);

    // --- SquashFS ------------------------------------------------

    pub const MKSQUASHFS: Tool = Tool::new("mksquashfs");
    pub const UNSQUASHFS: Tool = Tool::new("unsquashfs");

    // --- WIM (Windows Imaging Format) ---------------------------

    /// WIM tooling from wimlib. Linux: `wimtools` (Debian) /
    /// `wimlib-utils` (Fedora). macOS: `brew install wimlib`.
    pub const WIMLIB_IMAGEX: Tool = Tool::with_aliases("wimlib-imagex", &["imagex"]);

    // --- ext{2,3,4} ----------------------------------------------

    pub const MKFS_EXT4: Tool = Tool::new("mkfs.ext4");
    pub const E2FSCK: Tool = Tool::new("e2fsck");
    pub const DEBUGFS: Tool = Tool::new("debugfs");

    // --- NTFS ----------------------------------------------------

    /// NTFS author. Linux: `ntfs-3g` package. macOS: not commonly
    /// packaged; tests skip.
    pub const MKNTFS: Tool = Tool::new("mkntfs");
    pub const NTFSLS: Tool = Tool::new("ntfsls");
    pub const NTFSFIX: Tool = Tool::new("ntfsfix");

    // --- Compression and archiving (validation cross-checks) -----

    /// 7-Zip CLI. Used in `benches/seqread.rs` for the perf baseline
    /// and here as a third-party read-back validator (it knows most
    /// of the formats we do).
    pub const SEVEN_ZZ: Tool = Tool::with_aliases("7zz", &["7z"]);

    // --- Archive tools --------------------------------------------------

    /// System `zip` for building ZIP test archives.
    pub const ZIP: Tool = Tool::new("zip");
    /// System `unzip` for listing and extracting ZIP archives.
    pub const UNZIP: Tool = Tool::new("unzip");
    /// System `tar` for building TAR test archives.
    pub const TAR: Tool = Tool::new("tar");

    // --- Universal smoke-test tools ------------------------------
    //
    // These are present on every POSIX system without installation
    // and let `harness_self_test.rs` exercise the harness against
    // real binaries without depending on a format-tool install.

    pub const ECHO: Tool = Tool::new("echo");
    pub const TRUE_BIN: Tool = Tool::new("true");
    pub const FALSE_BIN: Tool = Tool::new("false");
    pub const DD: Tool = Tool::new("dd");
    pub const CAT: Tool = Tool::new("cat");
    pub const PRINTF: Tool = Tool::new("printf");
}
