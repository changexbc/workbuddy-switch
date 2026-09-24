import fs from 'node:fs/promises';
import path from 'node:path';
import {pathToFileURL} from 'node:url';
import assert from 'node:assert/strict';
import {chromium} from 'playwright';

const root = path.resolve(import.meta.dirname, '..');
const output = path.join(root, 'artifacts/branding');
await fs.mkdir(output, {recursive: true});
const image = (name, size, className = '') => `<img class="${className}" src="${pathToFileURL(path.join(root, 'src-tauri/icons', name))}" width="${size}" height="${size}" alt="${name}">`;
const row = (dark) => `<div class="tray ${dark ? 'dark' : ''}"><span>${dark ? 'Dark menu bar' : 'Light menu bar'}</span>${image('tray-icon-template.png', 18, dark ? 'invert' : '')}<span>18 pt / 2×</span>${image('tray-icon-color-16.png', 16)}${image('tray-icon-color.png', 32)}<span>Color 16 / 32 px</span></div>`;
const html = `<!doctype html><html lang="en"><meta charset="utf-8"><title>Agent Companion icon assets</title>
<style>*{box-sizing:border-box}body{margin:0;padding:42px;background:#eef1eb;color:#234334;font:16px system-ui}h1{margin:0 0 8px;font-size:30px}p{margin:0 0 24px;color:#536357}section{background:white;padding:24px;margin-bottom:20px;border-radius:18px}.apps{display:flex;gap:28px;align-items:center}.apps img{border-radius:20%}.tray{display:flex;align-items:center;gap:22px;padding:18px;background:#f5f5f5;border-radius:10px;margin:12px 0}.dark{background:#252827;color:#fff}.invert{filter:invert(1)}.details{display:flex;gap:24px;align-items:center}.detail{display:flex;gap:32px;align-items:center;padding:20px;background:#f5f5f5;border-radius:12px}.detail.dark{background:#252827}small{color:#536357}img{object-fit:contain}</style>
<h1>Agent Companion · Conversation-tail cat</h1><p>Approved first version — jade green, dark forest-green belly, warm yellow background.</p>
<section><h2>Application / Dock / favicon</h2><div class="apps">${image('256x256.png',256)}${image('128x128.png',128)}${image('64x64.png',64)}${image('32x32.png',32)}${image('16x16.png',16)}</div><small>Rounded presentation preview; shipped PNG artwork is unchanged.</small></section>
<section><h2>Tray at actual display size</h2>${row(false)}${row(true)}</section>
<section><h2>Template detail · enlarged from shipped 36 px</h2><div class="details"><div class="detail">${image('tray-icon-template.png',144)}</div><div class="detail dark">${image('tray-icon-template.png',144,'invert')}</div>${image('tray-icon-color.png',144)}</div><p>Transparent speech bubble and facial cutouts; macOS uses system template tint.</p></section></html>`;
await fs.writeFile(path.join(output, 'preview.html'), html);
const browser = await chromium.launch({headless: true});
try {
  const page = await browser.newPage({viewport: {width: 1100, height: 1120}, deviceScaleFactor: 2});
  await page.goto(pathToFileURL(path.join(output, 'preview.html')).href);
  const images = await page.locator('img').evaluateAll(async nodes => {
    await Promise.all(nodes.map(image => image.decode()));
    return nodes.map(image => ({src: image.alt, width: image.naturalWidth, height: image.naturalHeight}));
  });
  assert(images.every(image => image.width > 0 && image.width === image.height));
  await page.screenshot({path: path.join(output, 'preview.png'), fullPage: true});
  await fs.writeFile(path.join(output, 'qa.json'), JSON.stringify({passed: true, images, note: 'Browser preview only; system menu-bar tint still needs native acceptance.'}, null, 2));
  console.log(`PASS: ${images.length} image instances decoded; ${output}/preview.png`);
} finally {
  await browser.close();
}
