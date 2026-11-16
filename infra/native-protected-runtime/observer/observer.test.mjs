import { describe, expect, mock, test } from 'bun:test';
import { createHash } from 'node:crypto';

mock.module('@earendil-works/pi-coding-agent', () => ({ VERSION: '1.5.2' }));
const { toolRegistry } = await import('./observer.mjs');
const sha = value => 'sha256:' + createHash('sha256').update(value).digest('hex');
const direct = name => ({ name, exposure: 'direct', parameters: {}, sourceInfo: { path: `builtin:${name}` } });
const registry = (all, active = ['read']) => ({ getAllTools: () => all, getActiveTools: () => active });

describe('protected observer registered-tool guard', () => {
  test('accepts direct tools and emits the active registry with measured digests', () => {
    const result = toolRegistry(registry([direct('read')]));
    expect(result).toEqual({ active: ['read'], tools: [{
      name: 'read', exposure: 'direct',
      schema_digest: sha('cadence.tool.schema.v1\0{}'),
      implementation_digest: sha('cadence.tool.implementation.v1\0builtin:read\0read'),
      source_kind: 'builtin', source_path: 'builtin:read',
    }] });
  });
  for (const exposure of ['codemode', 'deferred', 'hidden', 'model-only', 'unknown', undefined]) {
    test(`rejects registered inactive ${String(exposure)} exposure before active filtering`, () => {
      const inactive = { ...direct('indirect'), exposure };
      const pi = registry([direct('read'), inactive]);
      expect(pi.getActiveTools()).not.toContain('indirect');
      expect(() => toolRegistry(pi)).toThrow('non-direct tool is registered');
    });
  }
  test('rejects active names absent from the registry', () => {
    expect(() => toolRegistry(registry([direct('read')], ['read', 'missing']))).toThrow('active tools and registry disagree');
  });
});
