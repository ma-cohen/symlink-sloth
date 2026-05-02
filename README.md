# Symlink Sloth

Sloth is a tiny CLI for quickly adding and removing symlinks to folders from the folder you are currently in.

It is useful when you keep related repos next to each other, or in another folder, and want to link some of them into the current project while you work.

```text
Repos/
  app/
  shared-ui/
  api-client/
```

Run `sloth add` inside `Repos/app` and choose `shared-ui` and `api-client` to create:

```text
app/
  shared-ui -> ../shared-ui
  api-client -> ../api-client
```

## Requirements

- Rust and Cargo

## Install From GitHub

Install the CLI globally from this GitHub repo:

```sh
cargo install --git https://github.com/ma-cohen/symlink-sloth sloth
```

After installation, the `sloth` command is available in your terminal:

```sh
sloth --help
```

Update an existing Sloth install to the latest version from GitHub:

```sh
cargo install --git https://github.com/ma-cohen/symlink-sloth sloth --force
```

The `--force` flag replaces the previously installed `sloth` binary.

## Usage

Link sibling folders into the current folder:

```sh
sloth add
```

Choose from another folder:

```sh
sloth add ~/repos
sloth add ../shared
sloth add /opt/projects
```

Link every available sibling folder without prompting:

```sh
sloth add --all
```

Preview what would be linked:

```sh
sloth add --dry-run
```

Remove selected symlinks from the current folder:

```sh
sloth remove
```

In interactive `add` and `remove` lists, press `/` to search and type to filter the list. The prompt shows `(search: /query)` while typing; press `Esc` to return to normal selection mode, where the prompt shows `(filter: /query)`. Press `Space` to select items, or press `Esc` from normal selection mode to cancel. To clear the filter, enter search again and delete the query with `Backspace`.

Remove all symlinks from the current folder:

```sh
sloth remove --all
```

Skip the confirmation prompt when removing multiple symlinks:

```sh
sloth remove --all --yes
```

Show symlinks and whether their targets exist:

```sh
sloth status
```

Aliases for `remove` are also available:

```sh
sloth rm
sloth delete
sloth unlink
```

## Safety

Sloth keeps the filesystem behavior conservative:

- It creates symlinks only in the current folder.
- It never overwrites an existing file or directory.
- It removes only paths that are actual symlinks.
- It asks for confirmation before removing multiple symlinks unless `--yes` is used.
- It creates relative symlinks so related folders can move together.

## Local Development

Build the CLI:

```sh
cargo build
```

Run the CLI locally:

```sh
cargo run -- --help
```

You can also link it globally while developing:

```sh
cargo install --path .
sloth --help
```

Run tests:

```sh
cargo test
```
