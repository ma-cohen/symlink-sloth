use std::{
    env,
    ffi::OsStr,
    fs,
    io::{self, IsTerminal},
    path::{Path, PathBuf},
    process,
};

use anyhow::{anyhow, Context, Result};
use clap::{Args, Parser, Subcommand};
use dialoguer::{
    console::{Key, Term},
    theme::ColorfulTheme,
    Confirm, MultiSelect,
};

#[derive(Parser)]
#[command(
    name = "sloth",
    version,
    about = "Add and remove symlinks to sibling repositories from your current folder."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Choose sibling folders and symlink them into the current folder.
    Add(AddArgs),
    /// Remove symlinks from the current folder.
    #[command(alias = "rm", alias = "delete", alias = "unlink")]
    Remove(RemoveArgs),
    /// Show symlinks in the current folder and whether their targets exist.
    Status,
}

#[derive(Args)]
struct AddArgs {
    /// How many parent levels to search upward.
    #[arg(short = 'l', long, default_value_t = 1, value_parser = parse_positive_usize)]
    levels: usize,
    /// Link every available folder without prompting.
    #[arg(long)]
    all: bool,
    /// Show what would be linked without changing anything.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args)]
struct RemoveArgs {
    /// Remove every symlink in the current folder.
    #[arg(long)]
    all: bool,
    /// Skip confirmation prompts.
    #[arg(short = 'y', long)]
    yes: bool,
    /// Show what would be removed without changing anything.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Clone, Debug)]
struct CandidateFolder {
    name: String,
    target_relative: PathBuf,
    destination_path: PathBuf,
    destination_exists: bool,
}

#[derive(Clone, Debug)]
struct SymlinkEntry {
    name: String,
    path: PathBuf,
    target_display: String,
    target_exists: bool,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("Error: {error:#}");
        process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Add(args) => run_add(args),
        Commands::Remove(args) => run_remove(args),
        Commands::Status => run_status(),
    }
}

fn run_add(args: AddArgs) -> Result<()> {
    let cwd = env::current_dir().context("failed to read current directory")?;
    let search_root = get_search_root(&cwd, args.levels)?;
    let candidates = get_candidate_folders(&cwd, &search_root)?;
    let linkable_candidates = candidates
        .iter()
        .filter(|candidate| !candidate.destination_exists)
        .cloned()
        .collect::<Vec<_>>();

    if candidates.is_empty() {
        println!(
            "No folder candidates found in {}.",
            display_path(&search_root)?
        );
        return Ok(());
    }

    if linkable_candidates.is_empty() {
        println!(
            "No folders can be linked from {}.",
            display_path(&search_root)?
        );
        println!("Every candidate already has a matching path in the current folder.");
        return Ok(());
    }

    let selected = if args.all {
        linkable_candidates
    } else {
        choose_folders_to_link(&linkable_candidates, &search_root)?
    };

    if selected.is_empty() {
        println!("No symlinks created.");
        return Ok(());
    }

    for candidate in selected {
        create_directory_symlink(&candidate, args.dry_run)?;
    }

    Ok(())
}

fn run_remove(args: RemoveArgs) -> Result<()> {
    let cwd = env::current_dir().context("failed to read current directory")?;
    let symlinks = get_symlinks(&cwd)?;

    if symlinks.is_empty() {
        println!("No symlinks found in the current folder.");
        return Ok(());
    }

    let selected = if args.all {
        symlinks
    } else {
        choose_symlinks_to_remove(&symlinks)?
    };

    if selected.is_empty() {
        println!("No symlinks removed.");
        return Ok(());
    }

    if selected.len() > 1 && !args.yes {
        assert_interactive("Use --yes to remove multiple symlinks in non-interactive shells.")?;
        let should_remove = Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt(format!(
                "Remove {} symlinks from the current folder?",
                selected.len()
            ))
            .default(false)
            .interact()
            .context("failed to read confirmation")?;

        if !should_remove {
            println!("No symlinks removed.");
            return Ok(());
        }
    }

    for symlink in selected {
        remove_symlink(&symlink, args.dry_run)?;
    }

    Ok(())
}

fn run_status() -> Result<()> {
    let cwd = env::current_dir().context("failed to read current directory")?;
    let symlinks = get_symlinks(&cwd)?;

    if symlinks.is_empty() {
        println!("No symlinks found in the current folder.");
        return Ok(());
    }

    println!("Symlinks in {}:", display_path(&cwd)?);

    for symlink in symlinks {
        let state = if symlink.target_exists {
            "ok"
        } else {
            "missing target"
        };

        println!("- {} -> {} ({state})", symlink.name, symlink.target_display);
    }

    Ok(())
}

fn choose_folders_to_link(
    candidates: &[CandidateFolder],
    search_root: &Path,
) -> Result<Vec<CandidateFolder>> {
    assert_interactive("Use --all to link every available folder in non-interactive shells.")?;

    let items = folder_selection_items(candidates);
    let selected_indexes = choose_folder_indexes(
        &format!(
            "Choose folders from {} to symlink here",
            display_path(search_root)?
        ),
        &items,
    )
    .context("failed to read folder selection")?;

    Ok(selected_indexes
        .into_iter()
        .map(|index| candidates[index].clone())
        .collect())
}

fn choose_folder_indexes(prompt: &str, items: &[String]) -> Result<Vec<usize>> {
    let term = Term::stderr();

    if !term.is_term() {
        return Err(anyhow!(
            "Use --all to link every available folder in non-interactive shells."
        ));
    }

    if items.is_empty() {
        return Ok(Vec::new());
    }

    let mut highlighted = 0;
    let mut checked = vec![false; items.len()];
    let mut rendered_lines = 0;

    term.hide_cursor()?;
    let _cursor_guard = CursorVisibilityGuard::new(&term);

    loop {
        clear_rendered_lines(&term, rendered_lines)?;
        rendered_lines = render_folder_selection(&term, prompt, items, &checked, highlighted)?;

        match term.read_key()? {
            Key::ArrowDown | Key::Tab | Key::Char('j') => {
                highlighted = (highlighted + 1) % items.len();
            }
            Key::ArrowUp | Key::BackTab | Key::Char('k') => {
                highlighted = (highlighted + items.len() - 1) % items.len();
            }
            Key::Char(' ') => {
                checked[highlighted] = !checked[highlighted];
            }
            Key::Char('a') => {
                let should_check = !checked.iter().all(|is_checked| *is_checked);
                checked.fill(should_check);
            }
            Key::Escape | Key::Char('q') => {
                clear_rendered_lines(&term, rendered_lines)?;
                return Ok(Vec::new());
            }
            Key::Enter => {
                clear_rendered_lines(&term, rendered_lines)?;
                return Ok(finalize_folder_selection(&checked, highlighted));
            }
            _ => {}
        }
    }
}

fn folder_selection_items(candidates: &[CandidateFolder]) -> Vec<String> {
    candidates
        .iter()
        .map(|candidate| candidate.name.clone())
        .collect()
}

fn render_folder_selection(
    term: &Term,
    prompt: &str,
    items: &[String],
    checked: &[bool],
    highlighted: usize,
) -> io::Result<usize> {
    term.write_line(&format!("? {prompt}"))?;
    term.write_line("  Space: check multiple | Enter: link checked or highlighted | q: cancel")?;

    for (index, item) in items.iter().enumerate() {
        let cursor = if index == highlighted { ">" } else { " " };
        let marker = if checked[index] { "[x]" } else { "[ ]" };

        term.write_line(&format!("{cursor} {marker} {item}"))?;
    }

    term.flush()?;
    Ok(items.len() + 2)
}

fn clear_rendered_lines(term: &Term, line_count: usize) -> io::Result<()> {
    if line_count > 0 {
        term.clear_last_lines(line_count)?;
    }

    Ok(())
}

fn finalize_folder_selection(checked: &[bool], highlighted: usize) -> Vec<usize> {
    let selected = checked
        .iter()
        .enumerate()
        .filter_map(|(index, is_checked)| is_checked.then_some(index))
        .collect::<Vec<_>>();

    if selected.is_empty() && highlighted < checked.len() {
        vec![highlighted]
    } else {
        selected
    }
}

struct CursorVisibilityGuard<'a> {
    term: &'a Term,
}

impl<'a> CursorVisibilityGuard<'a> {
    fn new(term: &'a Term) -> Self {
        Self { term }
    }
}

impl Drop for CursorVisibilityGuard<'_> {
    fn drop(&mut self) {
        let _ = self.term.show_cursor();
        let _ = self.term.flush();
    }
}

fn choose_symlinks_to_remove(symlinks: &[SymlinkEntry]) -> Result<Vec<SymlinkEntry>> {
    assert_interactive("Use --all to remove every symlink in non-interactive shells.")?;

    let items = symlinks
        .iter()
        .map(|symlink| format!("{} -> {}", symlink.name, symlink.target_display))
        .collect::<Vec<_>>();

    let selected_indexes = MultiSelect::with_theme(&ColorfulTheme::default())
        .with_prompt("Choose symlinks to remove")
        .items(&items)
        .interact()
        .context("failed to read symlink selection")?;

    Ok(selected_indexes
        .into_iter()
        .map(|index| symlinks[index].clone())
        .collect())
}

fn create_directory_symlink(candidate: &CandidateFolder, dry_run: bool) -> Result<()> {
    if path_exists(&candidate.destination_path)? {
        println!("Skipped {}: destination already exists.", candidate.name);
        return Ok(());
    }

    if dry_run {
        println!(
            "Would link {} -> {}",
            candidate.name,
            path_to_display(&candidate.target_relative)
        );
        return Ok(());
    }

    create_dir_symlink(&candidate.target_relative, &candidate.destination_path)
        .with_context(|| format!("failed to link {}", candidate.name))?;
    println!(
        "Linked {} -> {}",
        candidate.name,
        path_to_display(&candidate.target_relative)
    );

    Ok(())
}

fn remove_symlink(symlink: &SymlinkEntry, dry_run: bool) -> Result<()> {
    let metadata = fs::symlink_metadata(&symlink.path).ok();

    if !metadata
        .as_ref()
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
    {
        println!("Skipped {}: it is no longer a symlink.", symlink.name);
        return Ok(());
    }

    if dry_run {
        println!(
            "Would remove {} -> {}",
            symlink.name, symlink.target_display
        );
        return Ok(());
    }

    fs::remove_file(&symlink.path)
        .with_context(|| format!("failed to remove {}", symlink.path.display()))?;
    println!("Removed {}", symlink.name);

    Ok(())
}

fn get_candidate_folders(cwd: &Path, search_root: &Path) -> Result<Vec<CandidateFolder>> {
    let current_real_path = realpath_or_resolve(cwd)?;
    let mut entries = read_sorted_entries(search_root)?;
    let mut candidates = Vec::new();

    for entry in entries.drain(..) {
        if !entry
            .file_type()
            .with_context(|| format!("failed to read file type for {}", entry.path().display()))?
            .is_dir()
        {
            continue;
        }

        let target_path = entry.path();
        let target_real_path = realpath_or_resolve(&target_path)?;

        if target_real_path == current_real_path
            || is_path_inside(&current_real_path, &target_real_path)
        {
            continue;
        }

        let name = os_str_to_string(entry.file_name())?;
        let destination_path = cwd.join(&name);
        let target_relative =
            pathdiff::diff_paths(&target_path, cwd).unwrap_or_else(|| target_path.clone());

        candidates.push(CandidateFolder {
            name,
            target_relative,
            destination_exists: path_exists(&destination_path)?,
            destination_path,
        });
    }

    Ok(candidates)
}

fn get_symlinks(cwd: &Path) -> Result<Vec<SymlinkEntry>> {
    let mut entries = read_sorted_entries(cwd)?;
    let mut symlinks = Vec::new();

    for entry in entries.drain(..) {
        if !entry
            .file_type()
            .with_context(|| format!("failed to read file type for {}", entry.path().display()))?
            .is_symlink()
        {
            continue;
        }

        let symlink_path = entry.path();
        let target = fs::read_link(&symlink_path).ok();
        let target_path = target.as_ref().map(|target| absolutize_from(cwd, target));
        let target_exists = target_path
            .as_ref()
            .map(|path| path_exists(path))
            .transpose()?
            .unwrap_or(false);
        let target_display = target
            .as_ref()
            .map(|target| path_to_display(target))
            .unwrap_or_else(|| "unknown target".to_string());

        symlinks.push(SymlinkEntry {
            name: os_str_to_string(entry.file_name())?,
            path: symlink_path,
            target_display,
            target_exists,
        });
    }

    Ok(symlinks)
}

fn get_search_root(cwd: &Path, levels: usize) -> Result<PathBuf> {
    let mut search_root = cwd.to_path_buf();

    for _ in 0..levels {
        let next_root = search_root.parent().ok_or_else(|| {
            anyhow!(
                "Cannot search {levels} levels up from {}.",
                display_path(cwd).unwrap_or_else(|_| cwd.display().to_string())
            )
        })?;

        if next_root == search_root {
            return Err(anyhow!(
                "Cannot search {levels} levels up from {}.",
                display_path(cwd).unwrap_or_else(|_| cwd.display().to_string())
            ));
        }

        search_root = next_root.to_path_buf();
    }

    Ok(search_root)
}

fn parse_positive_usize(value: &str) -> std::result::Result<usize, String> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| "must be a positive integer".to_string())?;

    if parsed == 0 {
        return Err("must be a positive integer".to_string());
    }

    Ok(parsed)
}

fn read_sorted_entries(path: &Path) -> Result<Vec<fs::DirEntry>> {
    let mut entries = fs::read_dir(path)
        .with_context(|| format!("failed to read {}", path.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("failed to read entries from {}", path.display()))?;

    entries.sort_by_key(|entry| entry.file_name());
    Ok(entries)
}

fn path_exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("failed to inspect {}", path.display())),
    }
}

fn realpath_or_resolve(path: &Path) -> Result<PathBuf> {
    match fs::canonicalize(path) {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Ok(absolutize_from(&env::current_dir()?, path))
        }
        Err(error) => Err(error).with_context(|| format!("failed to resolve {}", path.display())),
    }
}

fn absolutize_from(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

fn is_path_inside(child_path: &Path, parent_path: &Path) -> bool {
    child_path
        .strip_prefix(parent_path)
        .map(|relative_path| !relative_path.as_os_str().is_empty())
        .unwrap_or(false)
}

fn assert_interactive(message: &str) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(anyhow!(message.to_string()));
    }

    Ok(())
}

#[cfg(unix)]
fn create_dir_symlink(target: &Path, destination: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, destination)
}

#[cfg(windows)]
fn create_dir_symlink(target: &Path, destination: &Path) -> io::Result<()> {
    std::os::windows::fs::symlink_dir(target, destination)
}

fn display_path(path: &Path) -> Result<String> {
    let cwd = env::current_dir().context("failed to read current directory")?;
    let relative_path = pathdiff::diff_paths(path, cwd).unwrap_or_else(|| path.to_path_buf());

    if relative_path.as_os_str().is_empty() {
        return Ok(".".to_string());
    }

    if relative_path.starts_with("..") {
        return Ok(path_to_display(&relative_path));
    }

    Ok(format!(
        ".{}{}",
        std::path::MAIN_SEPARATOR,
        path_to_display(&relative_path)
    ))
}

fn path_to_display(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn os_str_to_string(value: impl AsRef<OsStr>) -> Result<String> {
    value
        .as_ref()
        .to_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow!("path contains invalid UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn candidate_discovery_excludes_current_folder_and_existing_destinations() -> Result<()> {
        let temp = tempdir()?;
        let repos = temp.path();
        let app = repos.join("app");
        let shared = repos.join("shared-ui");

        fs::create_dir(&app)?;
        fs::create_dir(&shared)?;
        fs::write(app.join("shared-ui"), "already here")?;

        let candidates = get_candidate_folders(&app, repos)?;

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].name, "shared-ui");
        assert!(candidates[0].destination_exists);
        assert_eq!(candidates[0].target_relative, PathBuf::from("../shared-ui"));

        Ok(())
    }

    #[test]
    fn search_root_walks_up_requested_levels() -> Result<()> {
        let temp = tempdir()?;
        let nested = temp.path().join("one").join("two");
        fs::create_dir_all(&nested)?;

        assert_eq!(get_search_root(&nested, 1)?, temp.path().join("one"));
        assert_eq!(get_search_root(&nested, 2)?, temp.path());

        Ok(())
    }

    #[test]
    fn folder_selection_items_show_names_only() {
        let candidates = vec![CandidateFolder {
            name: "shared-ui".to_string(),
            target_relative: PathBuf::from("../shared-ui"),
            destination_path: PathBuf::from("shared-ui"),
            destination_exists: false,
        }];

        assert_eq!(folder_selection_items(&candidates), vec!["shared-ui"]);
    }

    #[test]
    fn enter_without_checked_folders_selects_highlighted_folder() {
        assert_eq!(
            finalize_folder_selection(&[false, false, false], 1),
            vec![1]
        );
    }

    #[test]
    fn checked_folders_take_precedence_over_highlighted_folder() {
        assert_eq!(
            finalize_folder_selection(&[true, false, true], 1),
            vec![0, 2]
        );
    }

    #[test]
    fn status_marks_missing_symlink_targets() -> Result<()> {
        let temp = tempdir()?;
        let missing_target = PathBuf::from("missing");
        let link_path = temp.path().join("missing-link");

        create_dir_symlink(&missing_target, &link_path)?;

        let symlinks = get_symlinks(temp.path())?;

        assert_eq!(symlinks.len(), 1);
        assert_eq!(symlinks[0].name, "missing-link");
        assert!(!symlinks[0].target_exists);

        Ok(())
    }

    #[test]
    fn remove_symlink_ignores_regular_files() -> Result<()> {
        let temp = tempdir()?;
        let file_path = temp.path().join("regular-file");
        fs::write(&file_path, "not a symlink")?;

        let symlink = SymlinkEntry {
            name: "regular-file".to_string(),
            path: file_path.clone(),
            target_display: "regular-file".to_string(),
            target_exists: false,
        };

        remove_symlink(&symlink, false)?;

        assert!(file_path.exists());
        Ok(())
    }
}
