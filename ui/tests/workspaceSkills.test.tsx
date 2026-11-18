import { describe, it } from 'node:test';
import assert from 'node:assert';
import { usedByCounts } from '../src/features/agents/workspaceSkills';

describe('WorkspaceSkills - usedByCounts', () => {
  it('correctly calculates counts', () => {
    const users = [
      { profile_id: '1', name: 'A', status: 'active' },
      { profile_id: '2', name: 'B', status: 'archived' },
      { profile_id: '3', name: 'C', status: 'active' },
    ] as any;
    const counts = usedByCounts(users);
    assert.strictEqual(counts.active, 2);
    assert.strictEqual(counts.archived, 1);
    assert.strictEqual(counts.total, 3);
  });
});
