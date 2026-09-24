import http from 'node:http';
import fs from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createCollector } from './lib/collector.js';
import { openDesktopSource } from './lib/open-app.js';
export async function startServer({ port = 8849, collector = createCollector(), staticRoot = fileURLToPath(new URL('../dist', import.meta.url)), openApp } = {}) {
  const clients = new Set();
  const server = http.createServer(async (req,res) => {
    const host = req.headers.host || '';
    if (!/^(127\.0\.0\.1|localhost)(:\d+)?$/.test(host)) { res.writeHead(403).end(); return; }
    if (req.headers.origin && req.headers.origin !== `http://${host}`) { res.writeHead(403).end(); return; }
    let url; try { url = decodeURIComponent(new URL(req.url, `http://${host}`).pathname); } catch { res.writeHead(400).end(); return; }
    res.setHeader('Cache-Control','no-store'); res.setHeader('X-Content-Type-Options','nosniff');
    if(url==='/api/settings'||url==='/api/settings/check'){
      if(!collector.getSettings){res.writeHead(503).end('Settings unavailable');return;}
      try{
        if(req.method==='GET'&&url==='/api/settings'){res.setHeader('Content-Type','application/json');res.end(JSON.stringify(collector.getSettings()));return;}
        if(req.method!=='PUT'&&req.method!=='POST'){res.writeHead(405).end();return;}
        if(!String(req.headers['content-type']).startsWith('application/json')){res.writeHead(415).end();return;}
        let body='';for await(const chunk of req){body+=chunk;if(Buffer.byteLength(body)>32768){res.writeHead(413).end();return;}}
        const data=JSON.parse(body);
        const result=url.endsWith('/check')?await collector.checkSource(data):await collector.updateSettings(data);
        res.setHeader('Content-Type','application/json');res.end(JSON.stringify(result));
      }catch(e){res.writeHead(400,{'Content-Type':'application/json'});res.end(JSON.stringify({error:e.message}));}return;
    }
    if ((req.method === 'GET' || req.method === 'POST') && url === '/api/open-session') {
      let parsed;
      try { parsed = new URL(req.url, `http://${host}`); } catch { res.writeHead(400).end(); return; }
      try {
        const result = await openDesktopSource(parsed.searchParams.get('source'), {
          ...(openApp ? { openApp } : {}),
          edition: parsed.searchParams.get('edition'),
        });
        res.writeHead(200, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify(result));
      } catch (e) {
        const status = e.code === 'UNSUPPORTED_SOURCE' ? 400 : e.code === 'APP_OPEN_UNSUPPORTED' ? 501 : 502;
        res.writeHead(status, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ ok: false, error: e.code || e.message }));
      }
      return;
    }
    if (url === '/api/custom-integrations' || url === '/api/custom-integrations/preview') {
      const preview = url.endsWith('/preview');
      if (!collector.customIntegrationsGet || !collector.customIntegrationsSet || !collector.customPreview) { res.writeHead(503).end('Custom integrations unavailable'); return; }
      const fail = e => { res.writeHead(400, {'Content-Type':'application/json'}); res.end(JSON.stringify({error: e.message})); };
      if (!preview && req.method === 'GET') {
        try { res.setHeader('Content-Type','application/json'); res.end(JSON.stringify(await collector.customIntegrationsGet())); } catch (e) { fail(e); }
        return;
      }
      if (req.method !== 'POST') { res.writeHead(405).end(); return; }
      if (!String(req.headers['content-type']).startsWith('application/json')) { res.writeHead(415).end(); return; }
      let body = '';
      try {
        for await (const chunk of req) { body += chunk; if (Buffer.byteLength(body) > 2 * 1024 * 1024) { res.writeHead(413).end(); return; } }
        const data = JSON.parse(body);
        const value = preview ? await collector.customPreview(data) : await collector.customIntegrationsSet(data);
        res.setHeader('Content-Type','application/json'); res.end(JSON.stringify(value));
      } catch (e) { fail(e); }
      return;
    }
    if (req.method === 'POST' && (url === collector.codegWebhookPath?.() || url === '/api/codex-hook' || url === '/api/workbuddy-hook' || url === '/api/codebuddy-ide-hook')) {
      if (url === collector.codegWebhookPath?.() && collector.getSettings?.().sources.codeg.enabled !== true) {
        res.writeHead(410).end(); return;
      }
      const chunks = []; let size = 0;
      try {
        for await (const chunk of req) { size += chunk.length; if (size > 65536) { res.writeHead(413).end(); return; } chunks.push(chunk); }
        const payload = JSON.parse(Buffer.concat(chunks).toString('utf8') || '{}');
        if (url === collector.codegWebhookPath?.()) {
          if (!await collector.ingestCodegHook?.(payload)) { res.writeHead(400).end(); return; }
        }
        else if (url === '/api/codebuddy-ide-hook') await collector.ingestCodebuddyIdeHook?.(payload);
        else if (url === '/api/workbuddy-hook') await collector.ingestWorkbuddyHook?.(payload);
        else await collector.ingestCodexHook?.(payload);
        res.writeHead(204).end();
      } catch { res.writeHead(400).end(); }
      return;
    }
    if (req.method === 'POST' && url === '/api/custom-hook') {
      // Development path for a manually configured hook; the packaged app uses
      // the runtime's `custom-hook` command over the local IPC endpoint.
      const chunks = []; let size = 0;
      try {
        for await (const chunk of req) { size += chunk.length; if (size > 2 * 1024 * 1024) { res.writeHead(413).end(); return; } chunks.push(chunk); }
        const payload = JSON.parse(Buffer.concat(chunks).toString('utf8') || '{}');
        const outcome = await collector.ingestCustomHook?.(payload);
        res.writeHead(200, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify(outcome ?? null));
      } catch (e) { res.writeHead(400, { 'Content-Type': 'application/json' }); res.end(JSON.stringify({ error: e.message })); }
      return;
    }
    if (req.method !== 'GET') { res.writeHead(405).end(); return; }
    if (url === '/api/state') { res.setHeader('Content-Type','application/json'); res.end(JSON.stringify(collector.hub.snapshot())); return; }
    if (url === '/events') {
      res.writeHead(200, {'Content-Type':'text/event-stream',Connection:'keep-alive','X-Accel-Buffering':'no'});
      res.write(`retry: 2000\ndata: ${JSON.stringify(collector.hub.snapshot())}\n\n`); clients.add(res);
      req.on('close',()=>clients.delete(res));res.on('error',()=>clients.delete(res)); return;
    }
    let root;try{root=await fs.realpath(staticRoot);}catch{res.writeHead(404).end('Run npm run build first.');return;}
    const file = path.resolve(root, '.' + (url === '/' ? '/desktop.html' : url));
    if (!file.startsWith(root + path.sep)) { res.writeHead(403).end(); return; }
    try {
      const actual = await fs.realpath(file);
      if (!actual.startsWith(root + path.sep)) { res.writeHead(403).end(); return; }
      const data = await fs.readFile(actual);
      const mime={'.html':'text/html; charset=utf-8','.js':'text/javascript; charset=utf-8','.css':'text/css; charset=utf-8','.json':'application/json','.glb':'model/gltf-binary','.png':'image/png','.svg':'image/svg+xml'};
      res.setHeader('Content-Type',mime[path.extname(file)] || 'application/octet-stream');res.end(data);
    } catch { res.writeHead(404).end('Not found. Run npm run build before npm start.'); }
  });
  await new Promise((resolve,reject)=>{server.once('error',reject);server.listen(port,'127.0.0.1',resolve);});
  collector.setMonitorUrl?.(`http://127.0.0.1:${server.address().port}`);
  try { await collector.start(); } catch(e) { server.close(); throw e; }
  const timer = setInterval(()=>{
    if (!clients.size) return;
    const frame=`data: ${JSON.stringify(collector.hub.snapshot())}\n\n`;
    for (const res of clients) { if (res.writableLength > 1024*1024) { res.destroy();clients.delete(res); } else res.write(frame); }
  },1000);
  return { server, collector, async close() { clearInterval(timer);await collector.stop();for(const r of clients)r.end();server.closeAllConnections();await new Promise(resolve=>server.close(resolve)); } };
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const runtime = await startServer({port:Number(process.env.MONITOR_PORT || 8849)});
  console.log(`Agent Studio monitor → http://127.0.0.1:${runtime.server.address().port}`);
  for(const signal of ['SIGINT','SIGTERM'])process.once(signal,async()=>{await runtime.close();process.exit(0);});
}
