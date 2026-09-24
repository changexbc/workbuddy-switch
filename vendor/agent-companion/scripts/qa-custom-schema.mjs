// Keeps the delivered artifacts in agreement: the JSON Schema, the importable
// examples, the shared fixture and the runtime validator must all describe the
// same v1 contract. ajv is resolved from the installed dependency tree (it is a
// dev-time dependency of the toolchain); the script fails loudly if it is gone.
import { readFileSync } from 'node:fs';
import assert from 'node:assert/strict';
import { validateTemplate } from '../collector/lib/custom.js';

const read = name => JSON.parse(readFileSync(new URL(name, import.meta.url), 'utf8'));
const schema = read('../docs/schemas/custom-agent-hooks-v1.json');
const fixture = read('../tests/fixtures/custom-hooks.json');
const examples = {
  generic: read('../docs/examples/custom-agent-hooks/generic-agent.json'),
  minimal: read('../docs/examples/custom-agent-hooks/minimal-agent.json'),
};

let Ajv;
try { ({default: Ajv} = await import('ajv')); } catch { throw Error('缺少 ajv，请先 npm install 后重试'); }
const ajv = new Ajv({allErrors: true, strict: false});
const validate = ajv.compile(schema);

function patch(value, steps) {
  const target = structuredClone(value);
  for (const step of steps) {
    const tokens = step.pointer.split('/').slice(1);
    let cursor = target;
    for (const token of tokens.slice(0, -1)) cursor = Array.isArray(cursor) ? cursor[Number(token)] : cursor[token];
    const last = tokens.at(-1);
    if (step.op === 'set') cursor[last] = structuredClone(step.value);
    else delete cursor[last];
  }
  return target;
}

// 1. The examples are importable: schema-valid, runtime-valid, and byte-identical
//    to the shared fixture templates the Rust and Node tests already run.
for (const [name, example] of Object.entries(examples)) {
  assert.ok(validate(example), `${name} example violates the v1 schema: ${ajv.errorsText(validate.errors)}`);
  const result = validateTemplate(example);
  assert.equal(result.ok, true, `${name} example must be accepted by the runtime: ${result.path}${result.message}`);
}
assert.deepEqual(examples.generic, fixture.templates.example, 'generic-agent.json must equal the shared fixture template');
assert.deepEqual(examples.minimal, fixture.templates.minimal, 'minimal-agent.json must equal the shared fixture template');

// 2. Everything the shared fixture treats as valid is schema-valid too.
for (const [name, template] of Object.entries(fixture.templates)) {
  assert.ok(validate(template), `fixture template ${name} violates the v1 schema: ${ajv.errorsText(validate.errors)}`);
}

// ajv 6 reports `dataPath` (`.a.b[0]`), ajv 8+ reports `instancePath` (`/a/b/0`).
// `required` and `additionalProperties` point at the parent object, so the
// offending member name comes from the error params.
const errorPath = error => {
  const raw = error.instancePath ?? error.dataPath ?? '';
  const base = raw
    .replace(/\['((?:[^'\\]|\\.)*)'\]/g, '/$1')
    .replace(/\[(\d+)\]/g, '/$1')
    .replace(/\.([^.[\]]+)/g, '/$1');
  const member = error.params?.missingProperty ?? error.params?.additionalProperty;
  return member ? `${base}/${member}` : base;
};

// 3. Every rejected fixture mutation is rejected by both, and the schema points
//    at the same part of the document (ajv may stop one level higher because it
//    reports the object that failed `oneOf`).
for (const entry of fixture.invalid) {
  const candidate = patch(fixture.base, entry.mutate);
  const runtime = validateTemplate(candidate);
  assert.equal(runtime.ok, false, `${entry.name} must still be rejected by the runtime`);
  assert.ok(!validate(candidate), `${entry.name} must also be rejected by the schema`);
  const path = validate.errors.map(errorPath).filter(Boolean);
  assert.ok(
    path.some(candidate => entry.path.startsWith(candidate) || candidate.startsWith(entry.path)),
    `${entry.name}: schema errors ${JSON.stringify(path)} do not mention ${entry.path}`,
  );
}
console.log(`PASS: schema, ${Object.keys(examples).length} examples, ${Object.keys(fixture.templates).length} fixture templates and ${fixture.invalid.length} rejected mutations agree`);
