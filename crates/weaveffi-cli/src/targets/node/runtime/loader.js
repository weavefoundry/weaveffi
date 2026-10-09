import { createRequire } from 'node:module';

// The native entry points: the N-API addon built from {{ADDON}}.c. A prebuilt
// copy comes first (from the `{{PACKAGE}}-<os>-<cpu>` package `npm install`
// picks for this platform, or under prebuilds/<os>-<cpu>/), then the one
// node-gyp compiles at install time when none matches.
function $loadAddon(name) {
  const require = createRequire(import.meta.url);
  const platform = `${process.platform}-${process.arch}`;
  const candidates = [
    `{{PACKAGE}}-${platform}/${name}`,
    `./prebuilds/${platform}/${name}`,
    `./build/Release/${name}`,
    `./build/Debug/${name}`,
  ];
  for (const candidate of candidates) {
    try {
      return require(candidate);
    } catch (e) {
      if (e.code !== 'MODULE_NOT_FOUND') throw e;
    }
  }
  throw new Error(
    `the native addon ${name} was not found (tried ${candidates.join(', ')}); ` +
      'install the package for this platform, or build the addon with `npm install` ' +
      'or `node-gyp rebuild` in this package',
  );
}

const $raw = $loadAddon('{{ADDON}}.node');
$raw.$setup($Fault);

// Object tokens in value buffers are already bigint handles here.
const $token = (t) => t;
