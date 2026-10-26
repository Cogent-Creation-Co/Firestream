// Fail when a directory tree holds machine code.
// Copyright Firestream. MIT License.
//
// Usage: node find-machine-code.cjs [--native-only] [--allow REGEX]... DIR...
//
// Prints each regular file under the DIRs that starts with an ELF, Mach-O,
// PE or WebAssembly header, and exits 1 when it finds one. Symlinks are not
// followed. The headers are the ones that
// bin/nix/firestream/supabase/scan-binaries.py reads.
//
//   --allow REGEX   skip a file whose path matches REGEX (repeatable)
//   --native-only   skip WebAssembly files
//
// NOTE: --native-only exists for a content-addressed store such as the pnpm
// store. Its file names are hashes, so an --allow pattern cannot name the
// WebAssembly file that a later step deletes from node_modules.

const fs = require('fs');
const path = require('path');

const MACHO = new Set(['feedface', 'cefaedfe', 'feedfacf', 'cffaedfe', 'cafebabe', 'bebafeca']);

function kind(file) {
  const fd = fs.openSync(file, 'r');
  try {
    const head = Buffer.alloc(64);
    const n = fs.readSync(fd, head, 0, 64, 0);
    if (n >= 4 && head.readUInt32BE(0) === 0x7f454c46) return 'ELF';
    if (n >= 4 && head.readUInt32BE(0) === 0x0061736d) return 'WASM';
    if (n >= 4 && MACHO.has(head.toString('hex', 0, 4))) return 'Mach-O';
    if (n === 64 && head.toString('latin1', 0, 2) === 'MZ') {
      const sig = Buffer.alloc(4);
      if (fs.readSync(fd, sig, 0, 4, head.readUInt32LE(0x3c)) === 4 && sig.toString('latin1') === 'PE\0\0') return 'PE';
    }
    return null;
  } finally {
    fs.closeSync(fd);
  }
}

const allow = [];
let nativeOnly = false;
const stack = [];
const argv = process.argv.slice(2);
for (let i = 0; i < argv.length; i += 1) {
  if (argv[i] === '--allow') {
    i += 1;
    if (i === argv.length) throw new Error('--allow needs a REGEX');
    allow.push(new RegExp(argv[i]));
  } else if (argv[i] === '--native-only') {
    nativeOnly = true;
  } else {
    stack.push(argv[i]);
  }
}
if (stack.length === 0) throw new Error('give at least one DIR');

let found = 0;
while (stack.length > 0) {
  const dir = stack.pop();
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const file = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      stack.push(file);
    } else if (entry.isFile()) {
      const k = kind(file);
      if (k === null || (nativeOnly && k === 'WASM') || allow.some((re) => re.test(file))) continue;
      console.log(`${k}\t${file}`);
      found += 1;
    }
  }
}
process.exit(found === 0 ? 0 : 1);
