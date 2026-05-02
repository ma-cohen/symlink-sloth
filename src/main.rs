use std::{
    env,
    ffi::OsStr,
    fmt, fs,
    io::{self, IsTerminal},
    path::{Path, PathBuf},
    process,
};

use anyhow::{anyhow, Context, Result};
use clap::{Args, Parser, Subcommand};
use dialoguer::{
    console::{measure_text_width, Key, Term},
    theme::{ColorfulTheme, Theme},
    Confirm,
};

#[derive(Parser)]
#[command(
    name = "sloth",
    version,
    about = "Add and remove symlinks to folders from your current folder."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Choose folders and symlink them into the current folder.
    Add(AddArgs),
    /// Remove symlinks from the current folder.
    #[command(alias = "rm", alias = "delete", alias = "unlink")]
    Remove(RemoveArgs),
    /// Show symlinks in the current folder and whether their targets exist.
    Status,
}

#[derive(Args)]
struct AddArgs {
    /// Folder whose child folders should be offered for linking.
    #[arg(value_name = "SOURCE")]
    source: Option<PathBuf>,
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
    let source_was_provided = args.source.is_some();
    let search_root = get_search_root(&cwd, args.source.as_deref())?;
    let show_target_paths = source_was_provided || !is_sibling_search(&cwd, &search_root);
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
        choose_folders_to_link(&linkable_candidates, &search_root, show_target_paths)?
    };

    if selected.is_empty() {
        println!("No symlinks created.");
        return Ok(());
    }

    for candidate in selected {
        create_directory_symlink(&candidate, args.dry_run, show_target_paths)?;
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
    show_target_paths: bool,
) -> Result<Vec<CandidateFolder>> {
    assert_interactive("Use --all to link every available folder in non-interactive shells.")?;

    let items = candidates
        .iter()
        .map(|candidate| candidate_link_display(candidate, show_target_paths))
        .collect::<Vec<_>>();

    let selected_indexes = FilteredMultiSelect::with_theme(&ColorfulTheme::default())
        .with_prompt(format!(
            "Choose folders from {} to symlink here",
            display_path(search_root)?
        ))
        .items(&items)
        .interact()
        .context("failed to read folder selection")?;

    Ok(selected_indexes
        .into_iter()
        .map(|index| candidates[index].clone())
        .collect())
}

fn choose_symlinks_to_remove(symlinks: &[SymlinkEntry]) -> Result<Vec<SymlinkEntry>> {
    assert_interactive("Use --all to remove every symlink in non-interactive shells.")?;

    let items = symlinks
        .iter()
        .map(|symlink| format!("{} -> {}", symlink.name, symlink.target_display))
        .collect::<Vec<_>>();

    let selected_indexes = FilteredMultiSelect::with_theme(&ColorfulTheme::default())
        .with_prompt("Choose symlinks to remove")
        .items(&items)
        .interact()
        .context("failed to read symlink selection")?;

    Ok(selected_indexes
        .into_iter()
        .map(|index| symlinks[index].clone())
        .collect())
}

fn create_directory_symlink(
    candidate: &CandidateFolder,
    dry_run: bool,
    show_target_paths: bool,
) -> Result<()> {
    if path_exists(&candidate.destination_path)? {
        println!("Skipped {}: destination already exists.", candidate.name);
        return Ok(());
    }

    if dry_run {
        println!(
            "Would link {}",
            candidate_link_display(candidate, show_target_paths)
        );
        return Ok(());
    }

    create_dir_symlink(&candidate.target_relative, &candidate.destination_path)
        .with_context(|| format!("failed to link {}", candidate.name))?;
    println!(
        "Linked {}",
        candidate_link_display(candidate, show_target_paths)
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

fn get_search_root(cwd: &Path, source: Option<&Path>) -> Result<PathBuf> {
    let home = home_dir();
    get_search_root_with_home(cwd, source, home.as_deref())
}

fn get_search_root_with_home(
    cwd: &Path,
    source: Option<&Path>,
    home: Option<&Path>,
) -> Result<PathBuf> {
    let search_root = match source {
        Some(source) => resolve_source_path(cwd, source, home)?,
        None => cwd.parent().map(Path::to_path_buf).ok_or_else(|| {
            anyhow!(
                "Cannot search sibling folders from {} because it has no parent.",
                display_path(cwd).unwrap_or_else(|_| cwd.display().to_string())
            )
        })?,
    };

    ensure_directory(&search_root)?;

    Ok(search_root)
}

fn resolve_source_path(cwd: &Path, source: &Path, home: Option<&Path>) -> Result<PathBuf> {
    if is_home_relative_path(source) {
        let home = home.ok_or_else(|| {
            anyhow!(
                "Cannot expand {} because HOME is not set.",
                path_to_display(source)
            )
        })?;
        return Ok(expand_home_path(source, home));
    }

    if source.is_absolute() {
        Ok(source.to_path_buf())
    } else {
        Ok(cwd.join(source))
    }
}

fn is_home_relative_path(path: &Path) -> bool {
    matches!(
        path.components().next(),
        Some(std::path::Component::Normal(component)) if component == OsStr::new("~")
    )
}

fn expand_home_path(path: &Path, home: &Path) -> PathBuf {
    let mut expanded = home.to_path_buf();

    for component in path.components().skip(1) {
        expanded.push(component.as_os_str());
    }

    expanded
}

fn ensure_directory(path: &Path) -> Result<()> {
    let metadata =
        fs::metadata(path).with_context(|| format!("failed to inspect {}", path.display()))?;

    if !metadata.is_dir() {
        return Err(anyhow!("{} is not a directory.", path.display()));
    }

    Ok(())
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn is_sibling_search(cwd: &Path, search_root: &Path) -> bool {
    cwd.parent()
        .map(|parent| parent == search_root)
        .unwrap_or(false)
}

fn candidate_link_display(candidate: &CandidateFolder, show_target_paths: bool) -> String {
    if show_target_paths {
        format!(
            "{} -> {}",
            candidate.name,
            path_to_display(&candidate.target_relative)
        )
    } else {
        candidate.name.clone()
    }
}

#[derive(Clone)]
struct FilteredMultiSelect<'a> {
    defaults: Vec<bool>,
    items: Vec<String>,
    prompt: Option<String>,
    report: bool,
    clear: bool,
    max_length: Option<usize>,
    theme: &'a dyn Theme,
}

impl<'a> FilteredMultiSelect<'a> {
    fn with_theme(theme: &'a dyn Theme) -> Self {
        Self {
            defaults: Vec::new(),
            items: Vec::new(),
            prompt: None,
            report: true,
            clear: true,
            max_length: None,
            theme,
        }
    }

    fn with_prompt<S: Into<String>>(mut self, prompt: S) -> Self {
        self.prompt = Some(prompt.into());
        self
    }

    fn items<T, I>(mut self, items: I) -> Self
    where
        T: ToString,
        I: IntoIterator<Item = T>,
    {
        for item in items {
            self.items.push(item.to_string());
            self.defaults.push(false);
        }

        self
    }

    fn interact(self) -> Result<Vec<usize>> {
        self.interact_on(&Term::stderr())
    }

    fn interact_on(self, term: &Term) -> Result<Vec<usize>> {
        if !term.is_term() {
            return Err(io::Error::new(io::ErrorKind::NotConnected, "not a terminal").into());
        }

        if self.items.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "Empty list of items given to `FilteredMultiSelect`",
            )
            .into());
        }

        let mut render = PromptRenderer::new(term, self.theme);
        let mut checked = self.defaults.clone();
        let mut query = String::new();
        let mut search_mode = false;
        let mut visible_indexes = filtered_item_indexes(&self.items, &query);
        let mut selected_visible_index = 0;
        let mut current_page = 0;

        let _cursor_guard = CursorGuard::hide(term)?;

        loop {
            let capacity = page_capacity(term, self.max_length);
            normalize_cursor(
                visible_indexes.len(),
                capacity,
                &mut selected_visible_index,
                &mut current_page,
            );
            render.render_multi_select(
                self.prompt.as_deref(),
                search_prompt_state(&query, search_mode),
                &self.items,
                &visible_indexes,
                &checked,
                selected_visible_index,
                current_page,
                capacity,
            )?;
            term.flush()?;

            match term.read_key()? {
                Key::Char('/') if !search_mode => {
                    search_mode = true;
                }
                Key::Escape if search_mode => {
                    search_mode = false;
                }
                Key::Escape => {
                    if self.clear {
                        render.clear()?;
                    }

                    term.flush()?;

                    return Ok(Vec::new());
                }
                Key::Backspace if search_mode => {
                    query.pop();
                    visible_indexes = filtered_item_indexes(&self.items, &query);
                    selected_visible_index = 0;
                    current_page = 0;
                }
                Key::Char(character) if search_mode && !character.is_ascii_control() => {
                    query.push(character);
                    visible_indexes = filtered_item_indexes(&self.items, &query);
                    selected_visible_index = 0;
                    current_page = 0;
                }
                Key::ArrowDown | Key::Tab | Key::Char('j') if !visible_indexes.is_empty() => {
                    selected_visible_index = (selected_visible_index + 1) % visible_indexes.len();
                }
                Key::ArrowUp | Key::BackTab | Key::Char('k') if !visible_indexes.is_empty() => {
                    selected_visible_index = (selected_visible_index + visible_indexes.len() - 1)
                        % visible_indexes.len();
                }
                Key::ArrowLeft | Key::Char('h')
                    if page_count(visible_indexes.len(), capacity) > 1 =>
                {
                    let pages = page_count(visible_indexes.len(), capacity);
                    current_page = (current_page + pages - 1) % pages;
                    selected_visible_index = current_page * capacity;
                }
                Key::ArrowRight | Key::Char('l')
                    if page_count(visible_indexes.len(), capacity) > 1 =>
                {
                    let pages = page_count(visible_indexes.len(), capacity);
                    current_page = (current_page + 1) % pages;
                    selected_visible_index = current_page * capacity;
                }
                Key::Char(' ') if !visible_indexes.is_empty() => {
                    let original_index = visible_indexes[selected_visible_index];
                    checked[original_index] = !checked[original_index];
                }
                Key::Char('a') => {
                    if checked.iter().all(|&item_checked| item_checked) {
                        checked.fill(false);
                    } else {
                        checked.fill(true);
                    }
                }
                Key::Enter => {
                    if self.clear {
                        render.clear()?;
                    }

                    if let Some(ref prompt) = self.prompt {
                        if self.report {
                            let selections = checked
                                .iter()
                                .enumerate()
                                .filter_map(|(index, &is_checked)| {
                                    is_checked.then_some(self.items[index].as_str())
                                })
                                .collect::<Vec<_>>();
                            render.render_multi_select_selection(prompt, &selections)?;
                        }
                    }

                    term.flush()?;

                    return Ok(selected_item_indexes(&checked));
                }
                _ => {}
            }
        }
    }
}

struct CursorGuard<'a> {
    term: &'a Term,
}

impl<'a> CursorGuard<'a> {
    fn hide(term: &'a Term) -> Result<Self> {
        term.hide_cursor()?;
        Ok(Self { term })
    }
}

impl Drop for CursorGuard<'_> {
    fn drop(&mut self) {
        let _ = self.term.show_cursor();
        let _ = self.term.flush();
    }
}

struct PromptRenderer<'a> {
    term: &'a Term,
    theme: &'a dyn Theme,
    height: usize,
}

impl<'a> PromptRenderer<'a> {
    fn new(term: &'a Term, theme: &'a dyn Theme) -> Self {
        Self {
            term,
            theme,
            height: 0,
        }
    }

    fn clear(&mut self) -> Result<()> {
        if self.height > 0 {
            self.term.clear_last_lines(self.height)?;
            self.height = 0;
        }

        Ok(())
    }

    fn render_multi_select(
        &mut self,
        prompt: Option<&str>,
        search_state: SearchPromptState<'_>,
        items: &[String],
        visible_indexes: &[usize],
        checked: &[bool],
        selected_visible_index: usize,
        current_page: usize,
        capacity: usize,
    ) -> Result<()> {
        self.clear()?;

        if let Some(prompt) = prompt {
            self.write_formatted_line(|theme, buffer| {
                let prompt = prompt_with_search(prompt, search_state);
                theme.format_multi_select_prompt(buffer, &prompt)?;

                let pages = page_count(visible_indexes.len(), capacity);
                if pages > 1 {
                    write!(buffer, " [Page {}/{}] ", current_page + 1, pages)?;
                }

                Ok(())
            })?;
        }

        if visible_indexes.is_empty() {
            self.write_line("  No matches")?;
            return Ok(());
        }

        let page_start = current_page * capacity;
        for visible_index in page_start..(page_start + capacity).min(visible_indexes.len()) {
            let original_index = visible_indexes[visible_index];
            self.write_formatted_line(|theme, buffer| {
                theme.format_multi_select_prompt_item(
                    buffer,
                    &items[original_index],
                    checked[original_index],
                    selected_visible_index == visible_index,
                )
            })?;
        }

        Ok(())
    }

    fn render_multi_select_selection(&mut self, prompt: &str, selections: &[&str]) -> Result<()> {
        self.write_formatted_line(|theme, buffer| {
            theme.format_multi_select_prompt_selection(buffer, prompt, selections)
        })
    }

    fn write_line(&mut self, line: &str) -> Result<()> {
        self.term.write_line(line)?;
        self.height += rendered_line_height(line, self.term);
        Ok(())
    }

    fn write_formatted_line<F>(&mut self, format_line: F) -> Result<()>
    where
        F: FnOnce(&dyn Theme, &mut dyn fmt::Write) -> fmt::Result,
    {
        let mut buffer = String::new();
        format_line(self.theme, &mut buffer)
            .map_err(|error| io::Error::new(io::ErrorKind::Other, error))?;
        self.write_line(&buffer)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SearchPromptState<'a> {
    Hidden,
    Searching(&'a str),
    Filtering(&'a str),
}

fn filtered_item_indexes(items: &[String], query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();

    items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            (query.is_empty() || item.to_lowercase().contains(&query)).then_some(index)
        })
        .collect()
}

fn selected_item_indexes(checked: &[bool]) -> Vec<usize> {
    checked
        .iter()
        .enumerate()
        .filter_map(|(index, &is_checked)| is_checked.then_some(index))
        .collect()
}

fn search_prompt_state(query: &str, search_mode: bool) -> SearchPromptState<'_> {
    if search_mode {
        SearchPromptState::Searching(query)
    } else if query.is_empty() {
        SearchPromptState::Hidden
    } else {
        SearchPromptState::Filtering(query)
    }
}

fn prompt_with_search(prompt: &str, search_state: SearchPromptState<'_>) -> String {
    match search_state {
        SearchPromptState::Hidden => prompt.to_string(),
        SearchPromptState::Searching(query) => format!("{prompt} (search: /{query})"),
        SearchPromptState::Filtering(query) => format!("{prompt} (filter: /{query})"),
    }
}

fn page_capacity(term: &Term, max_length: Option<usize>) -> usize {
    let rows = term.size().0 as usize;
    max_length.unwrap_or(rows).min(rows).max(3) - 2
}

fn page_count(item_count: usize, capacity: usize) -> usize {
    if item_count == 0 {
        1
    } else {
        item_count.div_ceil(capacity)
    }
}

fn normalize_cursor(
    visible_count: usize,
    capacity: usize,
    selected_visible_index: &mut usize,
    current_page: &mut usize,
) {
    if visible_count == 0 {
        *selected_visible_index = 0;
        *current_page = 0;
        return;
    }

    *selected_visible_index = (*selected_visible_index).min(visible_count - 1);
    *current_page = *selected_visible_index / capacity;
}

fn rendered_line_height(line: &str, term: &Term) -> usize {
    let width = term.size().1.max(1) as usize;

    line.split('\n')
        .map(|line| measure_text_width(line).max(1).div_ceil(width))
        .sum()
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
    fn default_search_root_is_parent_folder() -> Result<()> {
        let temp = tempdir()?;
        let app = temp.path().join("app");
        fs::create_dir(&app)?;

        assert_eq!(get_search_root_with_home(&app, None, None)?, temp.path());

        Ok(())
    }

    #[test]
    fn relative_source_path_resolves_from_current_folder() -> Result<()> {
        let temp = tempdir()?;
        let app = temp.path().join("app");
        let repos = temp.path().join("repos");
        fs::create_dir(&app)?;
        fs::create_dir(&repos)?;

        assert_eq!(
            get_search_root_with_home(&app, Some(Path::new("../repos")), None)?,
            app.join("../repos")
        );

        Ok(())
    }

    #[test]
    fn absolute_source_path_is_used_as_search_root() -> Result<()> {
        let temp = tempdir()?;
        let app = temp.path().join("app");
        let repos = temp.path().join("repos");
        fs::create_dir(&app)?;
        fs::create_dir(&repos)?;

        assert_eq!(
            get_search_root_with_home(&app, Some(repos.as_path()), None)?,
            repos
        );

        Ok(())
    }

    #[test]
    fn home_relative_source_path_expands_to_home_folder() -> Result<()> {
        let temp = tempdir()?;
        let app = temp.path().join("app");
        let home = temp.path().join("home");
        let repos = home.join("repos");
        fs::create_dir(&app)?;
        fs::create_dir(&home)?;
        fs::create_dir(&repos)?;

        assert_eq!(
            get_search_root_with_home(&app, Some(Path::new("~/repos")), Some(&home))?,
            repos
        );

        Ok(())
    }

    #[test]
    fn sibling_search_uses_name_only_for_candidate_display() -> Result<()> {
        let temp = tempdir()?;
        let app = temp.path().join("app");
        fs::create_dir(&app)?;

        assert!(is_sibling_search(&app, temp.path()));

        let candidate = CandidateFolder {
            name: "shared-ui".to_string(),
            target_relative: PathBuf::from("../shared-ui"),
            destination_path: app.join("shared-ui"),
            destination_exists: false,
        };

        assert_eq!(candidate_link_display(&candidate, false), "shared-ui");
        assert_eq!(
            candidate_link_display(&candidate, true),
            "shared-ui -> ../shared-ui"
        );

        Ok(())
    }

    #[test]
    fn non_sibling_search_keeps_candidate_target_path_visible() -> Result<()> {
        let temp = tempdir()?;
        let nested = temp.path().join("one").join("two");
        fs::create_dir_all(&nested)?;

        assert!(!is_sibling_search(&nested, temp.path()));

        Ok(())
    }

    #[test]
    fn empty_filter_keeps_every_item_index() {
        let items = vec![
            "api-client".to_string(),
            "shared-ui".to_string(),
            "website".to_string(),
        ];

        assert_eq!(filtered_item_indexes(&items, ""), vec![0, 1, 2]);
        assert_eq!(filtered_item_indexes(&items, "   "), vec![0, 1, 2]);
    }

    #[test]
    fn filter_matches_case_insensitive_substrings() {
        let items = vec![
            "api-client".to_string(),
            "Shared-UI".to_string(),
            "website".to_string(),
        ];

        assert_eq!(filtered_item_indexes(&items, "UI"), vec![1]);
        assert_eq!(filtered_item_indexes(&items, "site"), vec![2]);
    }

    #[test]
    fn filter_preserves_original_indexes() {
        let items = vec![
            "api-client".to_string(),
            "shared-ui".to_string(),
            "api-server".to_string(),
        ];

        assert_eq!(filtered_item_indexes(&items, "api"), vec![0, 2]);
        assert_eq!(filtered_item_indexes(&items, "worker"), Vec::<usize>::new());
    }

    #[test]
    fn selected_indexes_preserve_checked_items_when_filter_changes() {
        let checked = vec![true, false, true, false];

        assert_eq!(selected_item_indexes(&checked), vec![0, 2]);
    }

    #[test]
    fn prompt_shows_when_search_mode_is_active() {
        assert_eq!(
            prompt_with_search("Choose folders", SearchPromptState::Searching("")),
            "Choose folders (search: /)"
        );
        assert_eq!(
            prompt_with_search("Choose folders", SearchPromptState::Searching("api")),
            "Choose folders (search: /api)"
        );
    }

    #[test]
    fn prompt_shows_when_filter_is_active_but_search_mode_is_not() {
        assert_eq!(
            prompt_with_search("Choose folders", search_prompt_state("api", false)),
            "Choose folders (filter: /api)"
        );
        assert_eq!(
            prompt_with_search("Choose folders", search_prompt_state("", false)),
            "Choose folders"
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
