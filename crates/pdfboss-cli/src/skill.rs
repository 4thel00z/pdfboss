//! `pdfboss skill`: installing the bundled Claude Code skill, which teaches
//! coding agents the CLI, Python and Rust surfaces of pdfboss.

use std::fs;
use std::path::{Path, PathBuf};

use clap::Subcommand;

/// The skill document compiled into the binary, so the installed skill
/// always matches the installed pdfboss version.
const SKILL_MD: &str = include_str!("../skill/SKILL.md");

/// The nested subcommands of `pdfboss skill`.
#[derive(Subcommand)]
pub enum SkillCommand {
    /// Install the skill for coding agents: into ./.claude/skills/pdfboss
    /// (this project), or with --global into ~/.claude/skills/pdfboss.
    Install {
        /// Install into ~/.claude/skills instead of ./.claude/skills.
        #[arg(long, short = 'g')]
        global: bool,
    },
    /// Print the skill document to stdout.
    Show,
}

pub fn cmd_skill(command: SkillCommand) -> Result<(), String> {
    match command {
        SkillCommand::Install { global } => {
            let root = skills_root(global)?;
            let path = install_into(&root)?;
            println!("installed {}", path.display());
            Ok(())
        }
        SkillCommand::Show => {
            print!("{SKILL_MD}");
            Ok(())
        }
    }
}

/// The `.claude/skills` directory the flag selects: the home directory's
/// for `--global`, the current directory's otherwise.
fn skills_root(global: bool) -> Result<PathBuf, String> {
    if !global {
        return Ok(PathBuf::from(".claude").join("skills"));
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or_else(|| "cannot locate the home directory: neither HOME nor USERPROFILE is set".to_string())?;
    Ok(PathBuf::from(home).join(".claude").join("skills"))
}

/// Writes the skill under `root/pdfboss/SKILL.md`, creating directories as
/// needed and overwriting any previous install, and returns the file path.
fn install_into(root: &Path) -> Result<PathBuf, String> {
    let dir = root.join("pdfboss");
    fs::create_dir_all(&dir)
        .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let path = dir.join("SKILL.md");
    fs::write(&path, SKILL_MD).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pdfboss-skill-{tag}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn installs_and_overwrites() {
        let root = scratch_dir("install");
        let path = install_into(&root).unwrap();
        assert_eq!(path, root.join("pdfboss").join("SKILL.md"));
        let written = fs::read_to_string(&path).unwrap();
        assert_eq!(written, SKILL_MD);

        // A second install overwrites rather than failing.
        fs::write(&path, "stale").unwrap();
        install_into(&root).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), SKILL_MD);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn skill_document_has_frontmatter_and_no_em_dashes() {
        assert!(SKILL_MD.starts_with("---\nname: pdfboss\n"));
        assert!(SKILL_MD.contains("description:"));
        assert!(!SKILL_MD.contains('\u{2014}'));
    }

    #[test]
    fn local_root_is_relative_and_global_root_is_under_home() {
        assert_eq!(
            skills_root(false).unwrap(),
            PathBuf::from(".claude").join("skills")
        );
        let global = skills_root(true).unwrap();
        assert!(global.ends_with(Path::new(".claude").join("skills")));
        assert!(global.is_absolute());
    }
}
