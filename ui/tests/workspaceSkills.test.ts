// @ts-ignore
import { describe, it } from 'node:test';
// @ts-ignore
import assert from 'node:assert';
import { usedByCounts } from '../src/features/agents/workspaceSkills';
import type { WorkspaceSkillUser } from '../src/lib/types';

describe('WorkspaceSkills - usedByCounts', () => {
  it('correctly calculates counts', () => {
    const users: WorkspaceSkillUser[] = [
      { profile_id: '1', name: 'A', status: 'active' },
      { profile_id: '2', name: 'B', status: 'archived' },
      { profile_id: '3', name: 'C', status: 'active' },
    ];
    const counts = usedByCounts(users);
    assert.strictEqual(counts.active, 2);
    assert.strictEqual(counts.archived, 1);
    assert.strictEqual(counts.total, 3);
  });
});
