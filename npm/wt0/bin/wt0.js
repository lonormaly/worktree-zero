#!/usr/bin/env node
'use strict';

// Thin dispatcher: resolve the prebuilt `wt0` binary for this platform from
// one of the optionalDependencies platform packages, then exec it in place.
// No dependencies, so this file has to run on a bare Node >=18 install.

const path = require('path');
const os = require('os');
const { spawnSync } = require('child_process');

const PLATFORM_PACKAGES = {
  'darwin-arm64': 'wt0-darwin-arm64',
  'darwin-x64': 'wt0-darwin-x64',
  'linux-x64': 'wt0-linux-x64',
  'linux-arm64': 'wt0-linux-arm64',
  'win32-x64': 'wt0-win32-x64',
  'win32-arm64': 'wt0-win32-arm64',
};

function resolveBinary() {
  const key = `${process.platform}-${process.arch}`;
  const pkgName = PLATFORM_PACKAGES[key];
  const binName = process.platform === 'win32' ? 'wt0.exe' : 'wt0';

  if (!pkgName) {
    return {
      error: [
        `wt0: no prebuilt binary is published for this platform (${key}).`,
        'Install from source instead: Homebrew (brew install lonormaly/wt0/wt0),',
        'cargo-binstall (cargo binstall worktree-zero), or a manual download from',
        'https://github.com/lonormaly/worktree-zero/releases.',
      ].join(' '),
    };
  }

  let pkgJsonPath;
  try {
    pkgJsonPath = require.resolve(`${pkgName}/package.json`);
  } catch {
    return {
      error: [
        `wt0: the platform package "${pkgName}" for ${key} is not installed.`,
        'This usually means optional dependencies were skipped during install',
        '(e.g. --no-optional, --omit=optional, or an npm config that disables them)',
        `or that ${key} has no prebuilt binary yet. Reinstall with optional`,
        'dependencies enabled, or use Homebrew (brew install lonormaly/wt0/wt0),',
        'cargo-binstall (cargo binstall worktree-zero), or a manual download from',
        'https://github.com/lonormaly/worktree-zero/releases.',
      ].join(' '),
    };
  }

  return { bin: path.join(path.dirname(pkgJsonPath), 'bin', binName) };
}

function exitCodeForSignal(signal) {
  const num = os.constants.signals[signal];
  return 128 + (typeof num === 'number' ? num : 1);
}

function main() {
  const resolved = resolveBinary();
  if (resolved.error) {
    console.error(resolved.error);
    process.exit(1);
  }

  const result = spawnSync(resolved.bin, process.argv.slice(2), { stdio: 'inherit' });

  if (result.error) {
    console.error(`wt0: failed to run "${resolved.bin}": ${result.error.message}`);
    process.exit(1);
  }
  if (result.signal) {
    process.exit(exitCodeForSignal(result.signal));
  }
  process.exit(result.status === null ? 1 : result.status);
}

main();
