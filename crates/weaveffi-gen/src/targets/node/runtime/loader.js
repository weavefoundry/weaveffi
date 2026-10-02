import { createRequire } from 'node:module';

// The native entry points: the N-API addon `npm install` builds from
// {{ADDON}}.c (with node-gyp), or a prebuilt copy next to this file.
function $loadAddon(name) {
  const require = createRequire(import.meta.url);
  const tried = [];
  for (const dir of ['./build/Release/', './build/Debug/', './']) {
    try {
      return require(dir + name);
    } catch (e) {
      if (e.code !== 'MODULE_NOT_FOUND') throw e;
      tried.push(dir + name);
    }
  }
  throw new Error(
    `the native addon ${name} was not found (tried ${tried.join(', ')}); ` +
      'build it with `npm install` or `node-gyp rebuild` in this package',
  );
}

const $raw = $loadAddon('{{ADDON}}.node');
$raw.$setup($Fault);

// Object tokens in value buffers are already bigint handles here.
const $token = (t) => t;
