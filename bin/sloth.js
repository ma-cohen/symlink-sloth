#!/usr/bin/env node

import { checkbox, confirm } from '@inquirer/prompts';
import { Command, InvalidArgumentError } from 'commander';
import fs from 'node:fs/promises';
import path from 'node:path';
import process from 'node:process';

const program = new Command();

program
  .name('sloth')
  .description('Add and remove symlinks to sibling repositories from your current folder.')
  .version('0.1.0');

program
  .command('add')
  .description('Choose sibling folders and symlink them into the current folder.')
  .option('-l, --levels <count>', 'how many parent levels to search upward', parsePositiveInteger, 1)
  .option('--all', 'link every available folder without prompting')
  .option('--dry-run', 'show what would be linked without changing anything')
  .action(runAdd);

program
  .command('remove')
  .aliases(['rm', 'delete', 'unlink'])
  .description('Remove symlinks from the current folder.')
  .option('--all', 'remove every symlink in the current folder')
  .option('-y, --yes', 'skip confirmation prompts')
  .option('--dry-run', 'show what would be removed without changing anything')
  .action(runRemove);

program
  .command('status')
  .description('Show symlinks in the current folder and whether their targets exist.')
  .action(runStatus);

program.parseAsync(process.argv).catch((error) => {
  if (error?.name === 'ExitPromptError') {
    console.error('\nCancelled.');
  } else {
    console.error(`Error: ${error.message}`);
  }

  process.exitCode = 1;
});

async function runAdd(options) {
  const cwd = process.cwd();
  const levels = options.levels;
  const searchRoot = getSearchRoot(cwd, levels);
  const candidates = await getCandidateFolders(cwd, searchRoot);
  const linkableCandidates = candidates.filter((candidate) => !candidate.destinationExists);

  if (candidates.length === 0) {
    console.log(`No folder candidates found in ${displayPath(searchRoot)}.`);
    return;
  }

  if (linkableCandidates.length === 0) {
    console.log(`No folders can be linked from ${displayPath(searchRoot)}.`);
    console.log('Every candidate already has a matching path in the current folder.');
    return;
  }

  const selected = options.all
    ? linkableCandidates
    : await chooseFoldersToLink(candidates, searchRoot);

  if (selected.length === 0) {
    console.log('No symlinks created.');
    return;
  }

  for (const candidate of selected) {
    await createDirectorySymlink(candidate, options);
  }
}

async function runRemove(options) {
  const cwd = process.cwd();
  const symlinks = await getSymlinks(cwd);

  if (symlinks.length === 0) {
    console.log('No symlinks found in the current folder.');
    return;
  }

  const selected = options.all
    ? symlinks
    : await chooseSymlinksToRemove(symlinks);

  if (selected.length === 0) {
    console.log('No symlinks removed.');
    return;
  }

  if (selected.length > 1 && !options.yes) {
    const shouldRemove = await confirm({
      message: `Remove ${selected.length} symlinks from the current folder?`,
      default: false,
    });

    if (!shouldRemove) {
      console.log('No symlinks removed.');
      return;
    }
  }

  for (const symlink of selected) {
    await removeSymlink(symlink, options);
  }
}

async function runStatus() {
  const cwd = process.cwd();
  const symlinks = await getSymlinks(cwd);

  if (symlinks.length === 0) {
    console.log('No symlinks found in the current folder.');
    return;
  }

  console.log(`Symlinks in ${displayPath(cwd)}:`);

  for (const symlink of symlinks) {
    const state = symlink.targetExists ? 'ok' : 'missing target';
    console.log(`- ${symlink.name} -> ${symlink.targetDisplay} (${state})`);
  }
}

async function chooseFoldersToLink(candidates, searchRoot) {
  assertInteractive('Use --all to link every available folder in non-interactive shells.');

  return checkbox({
    message: `Choose folders from ${displayPath(searchRoot)} to symlink here`,
    pageSize: 12,
    choices: candidates.map((candidate) => ({
      name: `${candidate.name} -> ${candidate.targetRelative}`,
      value: candidate,
      disabled: candidate.destinationExists ? 'destination already exists' : false,
    })),
  });
}

async function chooseSymlinksToRemove(symlinks) {
  assertInteractive('Use --all to remove every symlink in non-interactive shells.');

  return checkbox({
    message: 'Choose symlinks to remove',
    pageSize: 12,
    choices: symlinks.map((symlink) => ({
      name: `${symlink.name} -> ${symlink.targetDisplay}`,
      value: symlink,
    })),
  });
}

async function createDirectorySymlink(candidate, options) {
  if (await pathExists(candidate.destinationPath)) {
    console.log(`Skipped ${candidate.name}: destination already exists.`);
    return;
  }

  if (options.dryRun) {
    console.log(`Would link ${candidate.name} -> ${candidate.targetRelative}`);
    return;
  }

  await fs.symlink(candidate.targetRelative, candidate.destinationPath, getDirectorySymlinkType());
  console.log(`Linked ${candidate.name} -> ${candidate.targetRelative}`);
}

async function removeSymlink(symlink, options) {
  const stats = await fs.lstat(symlink.path).catch(() => null);

  if (!stats?.isSymbolicLink()) {
    console.log(`Skipped ${symlink.name}: it is no longer a symlink.`);
    return;
  }

  if (options.dryRun) {
    console.log(`Would remove ${symlink.name} -> ${symlink.targetDisplay}`);
    return;
  }

  await fs.unlink(symlink.path);
  console.log(`Removed ${symlink.name}`);
}

async function getCandidateFolders(cwd, searchRoot) {
  const currentRealPath = await realpathOrResolve(cwd);
  const entries = await fs.readdir(searchRoot, { withFileTypes: true });
  const candidates = [];

  for (const entry of entries.sort((a, b) => a.name.localeCompare(b.name))) {
    if (!entry.isDirectory()) {
      continue;
    }

    const targetPath = path.join(searchRoot, entry.name);
    const targetRealPath = await realpathOrResolve(targetPath);

    if (targetRealPath === currentRealPath || isPathInside(currentRealPath, targetRealPath)) {
      continue;
    }

    const destinationPath = path.join(cwd, entry.name);

    candidates.push({
      name: entry.name,
      targetPath,
      targetRelative: path.relative(cwd, targetPath) || '.',
      destinationPath,
      destinationExists: await pathExists(destinationPath),
    });
  }

  return candidates;
}

async function getSymlinks(cwd) {
  const entries = await fs.readdir(cwd, { withFileTypes: true });
  const symlinks = [];

  for (const entry of entries.sort((a, b) => a.name.localeCompare(b.name))) {
    if (!entry.isSymbolicLink()) {
      continue;
    }

    const symlinkPath = path.join(cwd, entry.name);
    const target = await fs.readlink(symlinkPath).catch(() => null);
    const targetPath = target ? path.resolve(cwd, target) : null;

    symlinks.push({
      name: entry.name,
      path: symlinkPath,
      target,
      targetDisplay: target ?? 'unknown target',
      targetPath,
      targetExists: targetPath ? await pathExists(targetPath) : false,
    });
  }

  return symlinks;
}

function getSearchRoot(cwd, levels) {
  let searchRoot = cwd;

  for (let index = 0; index < levels; index += 1) {
    const nextRoot = path.dirname(searchRoot);

    if (nextRoot === searchRoot) {
      throw new Error(`Cannot search ${levels} levels up from ${displayPath(cwd)}.`);
    }

    searchRoot = nextRoot;
  }

  return searchRoot;
}

function parsePositiveInteger(value) {
  const parsed = Number(value);

  if (!Number.isInteger(parsed) || parsed < 1) {
    throw new InvalidArgumentError('must be a positive integer');
  }

  return parsed;
}

function isPathInside(childPath, parentPath) {
  const relativePath = path.relative(parentPath, childPath);
  return Boolean(relativePath) && !relativePath.startsWith('..') && !path.isAbsolute(relativePath);
}

function assertInteractive(message) {
  if (!process.stdin.isTTY || !process.stdout.isTTY) {
    throw new Error(message);
  }
}

async function pathExists(filePath) {
  try {
    await fs.lstat(filePath);
    return true;
  } catch (error) {
    if (error?.code === 'ENOENT') {
      return false;
    }

    throw error;
  }
}

async function realpathOrResolve(filePath) {
  try {
    return await fs.realpath(filePath);
  } catch (error) {
    if (error?.code === 'ENOENT') {
      return path.resolve(filePath);
    }

    throw error;
  }
}

function getDirectorySymlinkType() {
  return process.platform === 'win32' ? 'junction' : 'dir';
}

function displayPath(filePath) {
  const relativePath = path.relative(process.cwd(), filePath);

  if (!relativePath) {
    return '.';
  }

  if (relativePath.startsWith('..')) {
    return relativePath;
  }

  return `.${path.sep}${relativePath}`;
}
