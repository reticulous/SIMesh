#!/usr/bin/env node
/* Refresh vendor/planner-wasm from SIMesh's built planner-wasm.
 *
 *     node vendor/update-planner-wasm.mjs [<planner workspace>]
 *
 * The page renders a pack's base map with planner-wasm, and takes it as a
 * built package: `"planner-wasm": "file:vendor/planner-wasm"` in package.json.
 * The package directory lives here and is committed, so the page installs
 * and builds without a wasm toolchain.
 *
 * wasm-pack's output is four files, which planner-web embeds from
 * crates/planner-web/static. That directory has no package.json, so this
 * script copies the four files and writes one beside them. The workspace is
 * the argument, else SIMesh's own planner/. */
import { copyFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const HERE = dirname(fileURLToPath(import.meta.url))
const SIMESH = resolve(HERE, '..', '..', '..')
const planner = resolve(process.argv[2] || join(SIMESH, 'planner'))
const source = join(planner, 'crates', 'planner-web', 'static')
const target = join(HERE, 'planner-wasm')
const FILES = ['planner_wasm.js', 'planner_wasm_bg.wasm',
               'planner_wasm.d.ts', 'planner_wasm_bg.wasm.d.ts']

for (const file of FILES) {
  if (!existsSync(join(source, file))) {
    console.error(`update-planner-wasm: no ${file} in ${source} (build the planner first: simesh build planner)`)
    process.exit(1)
  }
}
mkdirSync(target, { recursive: true })
for (const file of FILES) copyFileSync(join(source, file), join(target, file))

/* The planner's workspace version, so the package says which planner it came from. */
/* The crate takes its version from the workspace, so the root manifest is read too. */
let version = '0.0.0'
for (const manifest of [join(planner, 'crates', 'planner-wasm', 'Cargo.toml'), join(planner, 'Cargo.toml')]) {
  try {
    const found = /^version\s*=\s*"([^"]+)"/m.exec(readFileSync(manifest, 'utf8'))
    if (found && found[1]) { version = found[1]; break }
  } catch { /* a version is a label here, not a requirement */ }
}

const pkg = {
  name: 'planner-wasm',
  version,
  description: "Sergey's planner-wasm, as wasm-pack builds it for the web: copied here by update-planner-wasm.mjs.",
  type: 'module',
  main: 'planner_wasm.js',
  types: 'planner_wasm.d.ts',
  exports: {
    '.': { types: './planner_wasm.d.ts', default: './planner_wasm.js' },
    './planner_wasm_bg.wasm': './planner_wasm_bg.wasm',
  },
  files: FILES,
  sideEffects: false,
}
writeFileSync(join(target, 'package.json'), JSON.stringify(pkg, null, 2) + '\n')
console.log(`update-planner-wasm: ${target} from ${source} (version ${version})`)
