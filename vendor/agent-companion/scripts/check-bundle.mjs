import fs from 'node:fs/promises';
import path from 'node:path';
import assert from 'node:assert/strict';
const root = path.resolve(import.meta.dirname, '..');
const pkg = JSON.parse(await fs.readFile(path.join(root, 'package.json')));
assert(!pkg.dependencies.three && !pkg.devDependencies.three, '3D dependency returned');
const files = await fs.readdir(path.join(root, 'dist'), {recursive: true});
assert(files.includes('desktop.html') && files.includes('desktop-settings.html'));
let bytes = 0;
for (const file of files) {
  const absolute = path.join(root, 'dist', file), stat = await fs.stat(absolute);
  if (!stat.isFile()) continue;
  bytes += stat.size;
  assert(!/\.(?:glb|gltf|exr|hdr|blend)$/i.test(file), `Scene asset: ${file}`);
  assert(!/(^|\/)(?:models|textures|lightmaps)(\/|$)/.test(file), `Scene directory: ${file}`);
  if (/\.(?:js|html|css)$/.test(file)) {
    const text = await fs.readFile(absolute, 'utf8');
    assert(!/three-core|three-loaders|WebGLRenderer|查看 3D Agent 工作室|打开 3D 办公室/.test(text), `Office dependency: ${file}`);
    for (const match of (file.endsWith('.html') ? text : '').matchAll(/(?:src|href)=["'](\/[^"'#?]+)["']/g)) {
      await fs.access(path.join(root, 'dist', match[1]));
    }
  }
}
console.log(`PASS: independent rail/settings bundle, ${bytes} bytes, no 3D resources`);
