"""Offline unit tests for the CAD-1436 acceptance driver's pure helpers.

Run: python3 -m unittest scripts/test_cad1436_driver.py
No daemon, no live turn, no network.
"""
import importlib.util
import json
import unittest
from pathlib import Path

_spec = importlib.util.spec_from_file_location(
    'driver', Path(__file__).with_name('cad1436-answer-turn-acceptance.py'))
d = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(d)

PRIMARY = 'global/pms/eva/stb-eva-hotel-interface-v1-0.md'
SECONDARY = 'global/pms/eva/stb-eva-hotel-interface-v0-14.md'
PRIMARY_REV = 'fnv1a:a8c45140c37768f2'
SECONDARY_REV = 'fnv1a:d83152e7782885cd'


def meta_text(path, rev):
    return f'exit=0\n{{"kind":"text","path":"{path}","rev":"{rev}","text":"body {{x}}"}}'


def bash(call_id, cmd, text, is_error=False):
    return {'id': call_id, 'tool': 'bash', 'arguments': {'command': cmd},
            'result': {'is_error': is_error, 'text': text, 'text_sha256': 'h'}}


def user(text):
    return {'type': 'message', 'message': {'role': 'user', 'content': [{'type': 'text', 'text': text}]}}


def assistant_call(cid, name, args):
    return {'type': 'message', 'message': {'role': 'assistant', 'content': [
        {'type': 'toolCall', 'id': cid, 'name': name, 'arguments': args}]}}


def tool_result(cid, name, text, is_error=False):
    return {'type': 'message', 'message': {'role': 'toolResult', 'toolCallId': cid,
            'toolName': name, 'isError': is_error,
            'content': [{'type': 'text', 'text': text}]}}


class StaticWikiDiscovery(unittest.TestCase):
    def test_listing_is_discovery_not_source_or_search(self):
        t = bash('ls1', 'cadence wiki ls global/pms/eva', '[]')
        self.assertTrue(d.analyze_call(t)['supported'])
        self.assertEqual(d.analyze_call(t)['discovery_paths'], ['global/pms/eva'])
        self.assertEqual(d.source_reads([t]), [])
        self.assertEqual(d.merged_candidates([t]), (0, 0, 0))
        self.assertEqual(d.bind_citations([(PRIMARY, PRIMARY_REV)], d.source_reads([t]))[0],
                         'unverifiable')

    def test_listing_cannot_supply_metadata_for_another_call(self):
        listing = bash('ls1', 'cadence wiki ls global/pms/eva',
                       meta_text(PRIMARY, PRIMARY_REV))
        reads = [bash(str(i), f'cadence wiki cat {PRIMARY} --meta', '') for i in range(3)]
        self.assertEqual(len(d.source_reads([listing] + reads)), 3)
        self.assertFalse(any(r['ok'] for r in d.source_reads([listing] + reads)))

    def test_unknown_or_dynamic_listing_is_unverifiable(self):
        for cmd in ('cadence wiki ls "$P"', 'cadence wiki ls global/pms/eva --unknown',
                    'cadence wiki ls ../secret', 'cadence wiki list global/pms/eva'):
            with self.subTest(cmd=cmd):
                self.assertFalse(d.classify_command(cmd)['supported'])
                self.assertIsNone(d.merged_candidates([bash('x', cmd, '[]')])[0])


class PromptAndSlicing(unittest.TestCase):
    def test_dispatched_prompt_carries_policy_and_verbatim_question(self):
        p = d.dispatched_prompt('/x/policy.md', 'What method and path check out an Infor folio?')
        self.assertIn('/x/policy.md', p)
        self.assertTrue(p.endswith('answer this exact question: What method and path check out an Infor folio?'))

    def test_slice_starts_at_exact_prompt_not_bootstrap(self):
        prompt = d.dispatched_prompt('/x/p.md', 'Q?')
        recs = [user('Cadence bootstrap: something'), assistant_call('b1', 'bash', {'command': 'cadence wiki search "x"'}),
                tool_result('b1', 'bash', 'boot'), user(prompt),
                assistant_call('q1', 'bash', {'command': 'cadence wiki cat global/pms/eva/a.md --meta'}),
                tool_result('q1', 'bash', 'text')]
        start, sl = d.question_slice(recs, prompt)
        self.assertEqual(start, 3)
        trace = d.tool_trace(sl)
        self.assertEqual([t['id'] for t in trace], ['q1'])
        prior = d.tool_trace(recs[:start])
        self.assertEqual([t['id'] for t in prior], ['b1'])

    def test_substring_of_prompt_is_not_a_match(self):
        prompt = d.dispatched_prompt('/x/p.md', 'Q?')
        recs = [user('Read /x/p.md for answering policy, then answer this exact question: Q? extra')]
        self.assertEqual(d.question_slice(recs, prompt), (None, None))

    def test_missing_prompt_is_unverifiable(self):
        self.assertEqual(d.question_slice([user('other')], d.dispatched_prompt('/x', 'Q')), (None, None))


class CitationBinding(unittest.TestCase):
    def reads_for(self, *items):
        return [{'path': p, 'ok': True, 'rev': r, 'call_id': 'c', 'result_sha256': 'h'} for p, r in items]

    def test_primary_and_secondary_both_bound_passes(self):
        cited = d.citations_in_answer(
            f'x @ rev {PRIMARY_REV} ... {SECONDARY} @ rev {SECONDARY_REV}'.replace(f'x @ rev {PRIMARY_REV}', f'{PRIMARY} @ rev {PRIMARY_REV}'))
        status, rows = d.bind_citations(cited, self.reads_for((PRIMARY, PRIMARY_REV), (SECONDARY, SECONDARY_REV)))
        self.assertEqual(status, 'pass', rows)

    def test_secondary_cited_without_same_turn_metadata_is_unverifiable(self):
        answer = (f'{PRIMARY} @ rev {PRIMARY_REV}; {SECONDARY} @ rev {SECONDARY_REV}')
        cited = d.citations_in_answer(answer)
        status, rows = d.bind_citations(cited, self.reads_for((PRIMARY, PRIMARY_REV)))
        self.assertEqual(status, 'unverifiable')
        bad = [r for r in rows if not r['bound']]
        self.assertEqual(bad[0]['path'], SECONDARY)

    def test_path_in_answer_without_read_is_not_evidence(self):
        cited = d.citations_in_answer(f'{PRIMARY} @ rev {PRIMARY_REV}')
        status, _ = d.bind_citations(cited, [])
        self.assertEqual(status, 'unverifiable')

    def test_wrong_revision_in_answer_fails(self):
        cited = d.citations_in_answer(f'{PRIMARY} @ rev fnv1a:deadbeef00')
        status, rows = d.bind_citations(cited, self.reads_for((PRIMARY, PRIMARY_REV)))
        self.assertEqual(status, 'fail')

    def test_no_citation_is_unverifiable(self):
        self.assertEqual(d.bind_citations([], [])[0], 'unverifiable')

    def test_read_metadata_extracted_for_the_same_path_only(self):
        t = meta_text(SECONDARY, SECONDARY_REV)
        self.assertEqual(d.meta_header(t, SECONDARY), SECONDARY_REV)
        self.assertIsNone(d.meta_header(t, PRIMARY))


class CandidateBudget(unittest.TestCase):
    def search_result(self, n, q='a', offset=0):
        # Real CLI schema: one JSON object per line with base, matches and the q echo.
        matches = [{'path': f'p{i}.md', 'rev': f'fnv1a:{i:08x}', 'line': 1, 'text': 'x'}
                   for i in range(offset, offset + n)]
        return json.dumps({'base': '', 'matches': matches, 'q': q})

    def test_merged_unique_candidates_counted_from_complete_results(self):
        trace = [bash('s1', 'cadence wiki search "a" --json', self.search_result(12, 'a')),
                 bash('s2', 'cadence wiki search "b" --json', self.search_result(12, 'b'))]
        merged, queries, bad = d.merged_candidates(trace)
        self.assertEqual((merged, queries, bad), (12, 2, 0))  # identical ids would dedupe

    def test_overflow_is_detected_by_budget_gate(self):
        status, det = d.budget_gate(3, 25, 2, 0)
        self.assertEqual(status, 'fail')
        self.assertTrue(any('merged' in i for i in det['issues']))

    def test_query_overflow_fails(self):
        status, _ = d.budget_gate(4, 1, 1, 0)
        self.assertEqual(status, 'fail')

    def test_reads_overflow_fails(self):
        status, _ = d.budget_gate(1, 1, 9, 0)
        self.assertEqual(status, 'fail')

    def test_unparseable_search_result_is_unverifiable(self):
        trace = [bash('s1', 'cadence wiki search "a" --json', '{"base":"","matches":[{"path":"p"')]
        merged, queries, bad = d.merged_candidates(trace)
        self.assertIsNone(merged)
        self.assertEqual(bad, 1)
        status, _ = d.budget_gate(queries, merged, 0, bad)
        self.assertEqual(status, 'unverifiable')

    def test_queries_without_result_do_not_count_as_candidates(self):
        trace = [bash('s1', 'cadence wiki search "a" --json', '')]
        merged, queries, bad = d.merged_candidates(trace)
        self.assertEqual(queries, 1)
        self.assertEqual(bad, 1)  # empty result is not a complete result
        self.assertIsNone(merged)


class FactsAndCleanup(unittest.TestCase):
    def test_a08_requires_mandatory_type_length_format(self):
        st, det = d.grade_facts('A08-eva-v1-0-checkin-format',
                                'checkIn and checkOut are fields.')
        self.assertEqual(st, 'ungraded')
        self.assertIn('mandatory', det['marker_missing'])
        self.assertIn('format YYYYMMDD', det['marker_missing'])

    def test_a14_typed_selected_items_and_locator_markers_remain_ungraded(self):
        ans = ('PreCheckin modifies selected items needed for pre-check-in. '
               'PreRegisterFlag is xs:boolean in PreCheckinRequest. See section 31.')
        st, det = d.grade_facts('A14-opera-precheckin-definition', ans)
        self.assertEqual(st, 'ungraded', det)
        self.assertEqual(det['marker_coverage'], 'complete')
        self.assertEqual(det['semantic_status'], 'ungraded')

    def test_a14_completes_check_in_claim_is_forbidden(self):
        st, _ = d.grade_facts('A14-opera-precheckin-definition',
                              'PreCheckin PreRegisterFlag boolean lightweight; it completes check-in.')
        self.assertEqual(st, 'fail')

    def test_cleanup_residue_lists_aliases_still_present(self):
        self.assertEqual(d.cleanup_residue(['a', 'b']), ['a', 'b'])
        self.assertEqual(d.cleanup_residue([]), [])

    def test_capture_accepts_known_noncore_gold_cases_only(self):
        gold = json.loads(d.GOLD.read_text())
        by_id = {c['id']: c for c in gold['cases']}
        known = ['U01-eva-room-assignment', 'U02-opera-cloud-auth-narrowed',
                 'V01-eva-v0-14-vs-v1-0-dates']
        self.assertEqual(d.invalid_case_ids(known, by_id, capture_only=True), [])
        self.assertEqual(d.invalid_case_ids(['not-a-gold-case'], by_id, capture_only=True),
                         ['not-a-gold-case'])

    def test_noncore_capture_keeps_question_trace_and_never_passes(self):
        prompt = d.dispatched_prompt(str(d.POLICY), 'Exact noncore question?')
        path, rev = 'global/pms/eva/captured.md', 'fnv1a:aaaaaaaaaaaaaaaa'
        meta = json.dumps({'kind': 'text', 'path': path, 'rev': rev, 'text': 'actual source body'})
        recs = [user(prompt), assistant_call('policy', 'read', {'path': str(d.POLICY)}),
                tool_result('policy', 'read', 'policy'),
                assistant_call('cat', 'bash', {'command': f'cadence wiki cat {path} --meta'}),
                tool_result('cat', 'bash', meta)]
        out = d.analyze_turn(None, recs, prompt, f'{path} @ rev {rev}: answer', 'completed')
        self.assertEqual(out['overall'], 'ungraded')
        self.assertEqual(out['gates']['facts'], 'ungraded')
        self.assertEqual(out['gates']['successful_required_sources'], 'ungraded')
        self.assertEqual(out['trace']['tool_calls'][1]['analysis']['cat_reads'][0]['text'],
                         'actual source body')

    def test_capture_keeps_frozen_core_fact_checks(self):
        status, detail = d.grade_facts(
            'A14-opera-precheckin-definition',
            'PreCheckin PreRegisterFlag xs:boolean modifies selected items; section 31 pre-check-in.')
        self.assertEqual(status, 'ungraded')
        self.assertEqual(detail['marker_coverage'], 'complete')
        self.assertEqual(detail['semantic_status'], 'ungraded')




class PairingValidation(unittest.TestCase):
    def test_valid_pairs_produce_no_problems(self):
        recs = [assistant_call('c1', 'bash', {'command': 'x'}), tool_result('c1', 'bash', 'ok')]
        self.assertEqual(d.pairing_problems(recs), [])

    def test_missing_result_orphan_and_duplicates_are_reported(self):
        cases = {
            'missing': [assistant_call('c1', 'bash', {'command': 'x'})],
            'orphan': [tool_result('c9', 'bash', 'x')],
            'dup_call': [assistant_call('c1', 'bash', {}), assistant_call('c1', 'bash', {}),
                         tool_result('c1', 'bash', 'x')],
            'dup_result': [assistant_call('c1', 'bash', {}), tool_result('c1', 'bash', 'x'),
                           tool_result('c1', 'bash', 'x')],
        }
        for name, recs in cases.items():
            with self.subTest(name):
                self.assertTrue(d.pairing_problems(recs), name)

    def test_unpaired_wiki_call_makes_trace_unverifiable_in_analysis(self):
        t = {'id': 'c1', 'tool': 'bash', 'arguments': {'command': 'cadence wiki search "a" --json'},
             'result': None}
        a = d.analyze_call(t)
        self.assertFalse(a['search_ok'])
        self.assertTrue(a['problems'])

    def test_unpaired_known_cat_still_counts_as_attempted_read(self):
        t = {'id': 'c1', 'tool': 'bash', 'arguments': {'command': 'cadence wiki cat global/x.md --meta'},
             'result': None}
        self.assertEqual(len(d.source_reads([t])), 1)
        self.assertFalse(d.source_reads([t])[0]['ok'])

    def test_clipped_nonwiki_read_view_does_not_invalidate_trace(self):
        t = {'id': 'r1', 'tool': 'read', 'arguments': {'path': '/tmp/cache.md'},
             'result': {'is_error': False, 'text': 'clipped view', 'details': {}}}
        self.assertEqual(d.analyze_call(t)['problems'], [])


class NativeHydration(unittest.TestCase):
    def setUp(self):
        import tempfile
        self.tmp = Path(tempfile.mkdtemp())
        self._old_root = d.NATIVE_ROOT
        d.NATIVE_ROOT = str(self.tmp)
        self.body = '[1] {"kind":"text","path":"global/x.md","rev":"fnv1a:aaaaaaaaaaaaaaaa","text":"b"}\nline2'
        self.native = self.tmp / 'pi-bash-0137351e35a8056c.log'
        self.native.write_text(self.body + '\nfull tail\n')
        self.footer = f'\n\n[Showing lines 1-2 of 3 (50.0KB limit). Full output: {self.native}]'

    def tearDown(self):
        d.NATIVE_ROOT = self._old_root
        import shutil
        shutil.rmtree(self.tmp, ignore_errors=True)

    def res(self, fop, text=None, truncated=True):
        return {'is_error': False, 'text': (text if text is not None else self.body) + self.footer,
                'text_sha256': 'h', 'details': {'truncation': {'truncated': truncated}, 'fullOutputPath': fop}}

    def test_valid_native_artifact_recovers_full_text(self):
        text, prov, why = d.complete_result_text(self.res(str(self.native)), 'c')
        self.assertIsNone(why)
        self.assertEqual(prov['mode'], 'recovered')
        self.assertIn('full tail', text)

    def test_missing_symlink_and_path_escape_are_rejected(self):
        outside = self.tmp.parent / 'pi-bash-deadbeef.log'
        outside.write_text(self.body)
        link = self.tmp / 'pi-bash-abcdef12.log'
        link.symlink_to(self.native)
        cases = {
            'missing': str(self.tmp / 'pi-bash-00000000.log'),
            'symlink': str(link),
            'path_escape': str(self.tmp / '..' / outside.name),
            'wrong_format': str(self.tmp / 'other.log'),
        }
        try:
            for name, fop in cases.items():
                with self.subTest(name):
                    text, prov, why = d.complete_result_text(self.res(fop), 'c')
                    self.assertIsNone(text)
                    self.assertTrue(why)
        finally:
            outside.unlink(missing_ok=True)

    def test_pointer_mismatch_without_structured_metadata_is_unverifiable(self):
        r = self.res(str(self.native))
        r['details'] = {}
        text, prov, why = d.complete_result_text(r, 'c')
        self.assertIsNone(text)
        self.assertTrue(why)

    def test_exact_zero_display_sentinel_recovers_native_stdout(self):
        meta = {'truncated': True, 'truncatedBy': 'bytes', 'content': '',
                'outputLines': 0, 'outputBytes': 0, 'totalBytes': self.native.stat().st_size}
        display = '(no output)' + self.footer
        result = {'is_error': False, 'text': display,
                  'details': {'truncation': meta, 'fullOutputPath': str(self.native)}}
        text, prov, why = d.complete_result_text(result, 'c')
        self.assertIsNone(why)
        self.assertEqual(text, self.native.read_text())
        self.assertEqual(prov['mode'], 'recovered')

    def test_zero_display_counterfeit_requires_exact_metadata_and_matching_size(self):
        base = {'truncated': True, 'truncatedBy': 'bytes', 'content': '',
                'outputLines': 0, 'outputBytes': 0, 'totalBytes': self.native.stat().st_size}
        display = '(no output)' + self.footer
        variants = [
            {},
            {'truncation': dict(base), 'fullOutputPath': str(self.tmp / 'pi-bash-deadbeef.log')},
            {'truncation': {**base, 'outputBytes': 1}, 'fullOutputPath': str(self.native)},
            {'truncation': {**base, 'totalBytes': self.native.stat().st_size + 1},
             'fullOutputPath': str(self.native)},
        ]
        for details in variants:
            with self.subTest(details=details):
                result = {'is_error': False, 'text': display, 'details': details}
                text, _prov, why = d.complete_result_text(result, 'c')
                self.assertTrue(why or text is None)

    def test_literal_no_output_in_native_body_never_falls_back_on_bad_metadata(self):
        self.native.write_text(self.body + '\n(no output)\nfull tail\n')
        footer = f'\n\n[Showing lines 1-2 of 3 (50.0KB limit). Full output: {self.native}]'
        details = {'truncation': {'truncated': True, 'truncatedBy': 'bytes', 'content': '',
                                  'outputLines': 0, 'outputBytes': 0,
                                  'totalBytes': self.native.stat().st_size + 1},
                   'fullOutputPath': str(self.native)}
        result = {'is_error': False, 'text': '(no output)' + footer, 'details': details}
        text, _prov, why = d.complete_result_text(result, 'c')
        self.assertIsNone(text)
        self.assertTrue(why)

    def test_zero_display_text_does_not_accept_nonempty_display_bytes(self):
        result = self.res(str(self.native), text='(no output) evil stdout')
        text, _prov, why = d.complete_result_text(result, 'c')
        self.assertIsNone(text)
        self.assertTrue(why)

    def test_truncated_meta_json_is_not_bound(self):
        cut = '{"kind":"text","path":"global/x.md","rev":"fnv1a:aaaa'
        _s, meta, _b = d.parse_objects(cut)
        self.assertEqual(meta, [])
        self.assertIsNone(d.meta_header(cut, 'global/x.md'))


class StandaloneProof(unittest.TestCase):
    """Only a standalone recognised call whose complete stdout is one root JSON object is proof."""
    PROMPT = 'Read /tmp/cad1436/work-pi/answering-policy.md for answering policy, then answer this exact question: Q?'
    REQ = 'global/pms/infor/infor-hms-hotel-operation-api-3-8-4-1.md'
    REV = 'fnv1a:a39fb80e562593fa'
    OTHER = 'global/pms/infor/endpoint-index.md'
    OREV = 'fnv1a:1111111111111111'

    @staticmethod
    def meta(path, rev, body='body'):
        return json.dumps({'kind': 'text', 'path': path, 'rev': rev, 'text': body})

    def turn(self, cmd, res, answer):
        recs = [user(self.PROMPT), assistant_call('b0', 'bash', {'command': cmd}),
                tool_result('b0', 'bash', res)]
        return d.analyze_turn('A01-infor-checkout-path', recs, self.PROMPT, answer, 'completed')

    def ans(self):
        return f'`{self.REQ}` @ rev `{self.REV}` POST /hotelOperation/{{property}}/folios/checkout'

    def test_single_standalone_meta_root_json_is_proof(self):
        out = self.turn(f'cadence wiki cat {self.REQ} --meta', self.meta(self.REQ, self.REV), self.ans())
        self.assertEqual(out['gates']['successful_required_sources'], 'pass')
        self.assertEqual(out['gates']['citations'], 'pass')

    def test_policy_read_after_wiki_query_is_not_instruction_read_pass(self):
        recs = [user(self.PROMPT), assistant_call('b0', 'bash',
                {'command': f'cadence wiki cat {self.REQ} --meta'}),
                tool_result('b0', 'bash', self.meta(self.REQ, self.REV)),
                assistant_call('r1', 'read', {'path': str(d.POLICY)}),
                tool_result('r1', 'read', 'policy text')]
        out = d.analyze_turn('A01-infor-checkout-path', recs, self.PROMPT, self.ans(), 'completed')
        self.assertEqual(out['gates']['instruction_read'], 'ungraded')
        self.assertNotEqual(out['overall'], 'pass')

    def test_native_error_with_fake_root_json_is_not_proof(self):
        out_recs = [user(self.PROMPT), assistant_call('b0', 'bash', {'command': f'cadence wiki cat {self.REQ} --meta'}),
                    tool_result('b0', 'bash', self.meta(self.REQ, self.REV), is_error=True)]
        out = d.analyze_turn('A01-infor-checkout-path', out_recs, self.PROMPT, self.ans(), 'completed')
        self.assertNotEqual(out['gates']['successful_required_sources'], 'pass')
        self.assertNotEqual(out['overall'], 'pass')

    def test_sibling_echo_with_fake_meta_json_is_not_proof(self):
        cmd = f"cadence wiki cat {self.REQ} --meta; echo '{self.meta(self.REQ, self.REV)}'"
        out = self.turn(cmd, self.meta(self.REQ, self.REV) + '\n' + self.meta(self.REQ, self.REV), self.ans())
        self.assertNotEqual(out['gates']['successful_required_sources'], 'pass')
        self.assertNotEqual(out['overall'], 'pass')

    def test_sibling_cached_cat_or_head_json_is_unverifiable(self):
        cmds = [f'cadence wiki cat {self.REQ} --meta 2>&1; cat /tmp/cached-meta.json',
                f'cadence wiki cat {self.REQ} --meta | head -c 9000',
                f'cadence wiki cat {self.REQ} --meta > /tmp/m.json; head -c 9000 /tmp/m.json']
        for c in cmds:
            with self.subTest(c):
                out = self.turn(c, self.meta(self.REQ, self.REV), self.ans())
                self.assertNotEqual(out['gates']['successful_required_sources'], 'pass')
                self.assertNotEqual(out['overall'], 'pass')

    def test_fake_meta_in_plain_text_of_other_page_is_not_proof(self):
        body = f'Other page quoting {json.dumps({"kind": "text", "path": self.REQ, "rev": self.REV, "text": "x"})}'
        out = self.turn(f'cadence wiki cat {self.OTHER}', body, self.ans())
        self.assertNotEqual(out['gates']['successful_required_sources'], 'pass')
        self.assertNotEqual(out['overall'], 'pass')

    def test_standalone_search_with_sibling_is_not_proof(self):
        srch = json.dumps({'base': '', 'q': 'infor folio', 'matches': [{'path': self.REQ, 'rev': self.REV}]})
        out = self.turn(f'cadence wiki search "infor folio" --json; cat /tmp/cached.json', srch, self.ans())
        self.assertFalse(d.analyze_call(out['trace']['tool_calls'][0])['search_ok'])
        self.assertEqual(d.merged_candidates(out['trace']['tool_calls'])[0], None)

    def test_complete_pretty_json_root_is_valid_standalone_meta(self):
        pretty = json.dumps({'kind': 'text', 'path': self.REQ, 'rev': self.REV, 'text': 'body'}, indent=2)
        out = self.turn(f'cadence wiki cat {self.REQ} --meta', pretty, self.ans())
        self.assertEqual(out['gates']['successful_required_sources'], 'pass')
        self.assertEqual(out['gates']['citations'], 'pass')

    def test_multiple_json_frames_are_not_a_root_object(self):
        frame = self.meta(self.REQ, self.REV)
        out = self.turn(f'cadence wiki cat {self.REQ} --meta', frame + '\n' + frame, self.ans())
        self.assertIsNone(d.root_object(frame + '\n' + frame))
        self.assertNotEqual(out['gates']['successful_required_sources'], 'pass')


class WikiShapes(unittest.TestCase):
    PROMPT = 'Read /tmp/cad1436/work-pi/answering-policy.md for answering policy, then answer this exact question: Q?'

    def test_two_queries_in_one_command_need_two_result_objects(self):
        cmd = 'cadence wiki search "a" --json; cadence wiki search "b" --json'
        one = json.dumps({'base': '', 'matches': [], 'q': 'a'})
        t = bash('s', cmd, one)
        a = d.analyze_call(t)
        self.assertEqual(a['searches'], 2)
        self.assertFalse(a['search_ok'])
        self.assertEqual(d.merged_candidates([t])[0], None)

    def test_duplicate_cat_invocations_are_each_counted(self):
        cmd = 'cadence wiki cat global/x.md --meta; cadence wiki cat global/x.md --meta'
        meta = '{"kind":"text","path":"global/x.md","rev":"fnv1a:aaaaaaaaaaaaaaaa","text":"b"}'
        t = bash('c', cmd, meta + '\n' + meta)
        reads = d.source_reads([t])
        self.assertEqual(len(reads), 2)  # both still counted
        self.assertFalse(any(r['ok'] for r in reads))  # but shared stdout is not proof

    def test_variable_loop_and_unknown_subcommands_are_unsupported(self):
        cmds = [
            'cadence wiki cat $P --meta',
            'for p in global/a.md global/b.md; do cadence wiki cat $p; done',
            'cadence wiki frob x',
            'cadence wiki cat "$(echo global/a.md)"',
            'bash -c "cadence wiki cat global/a.md"',
        ]
        for c in cmds:
            with self.subTest(c):
                self.assertFalse(d.classify_command(c)['supported'])
                t = bash('c', c, 'x')
                self.assertFalse(d.analyze_call(t)['supported'] and d.analyze_call(t)['cat_reads'])

    def test_unsupported_shape_is_not_counted_as_zero(self):
        t = bash('c', 'cadence wiki cat $P --meta', 'x')
        self.assertEqual(d.merged_candidates([t]), (None, 0, 1))

    def test_unknown_dynamic_read_stays_unverifiable_not_zero(self):
        out = d.analyze_turn('A01-infor-checkout-path',
                             [user(self.PROMPT), assistant_call('c', 'bash',
                              {'command': 'cadence wiki cat "$PATH" --meta'}),
                              tool_result('c', 'bash', 'not evidence')],
                             self.PROMPT, 'x', 'completed')
        self.assertEqual(out['budget']['merged'], None)
        self.assertEqual(out['gates']['budget'], 'unverifiable')
        self.assertEqual(out['budget']['source_reads'], 0)

    def test_help_invocation_is_not_a_source_read(self):
        t = bash('c', 'cadence wiki cat --help', 'usage')
        a = d.analyze_call(t)
        self.assertFalse(a['cat_reads'])
        self.assertEqual(a['searches'], 0)

    def test_known_searches_in_unsupported_compound_still_count(self):
        cmd = '; '.join(f'cadence wiki search "q{i}" --json' for i in range(4)) + '; echo $?'
        out = d.analyze_turn('A01-infor-checkout-path',
                             [user(self.PROMPT), assistant_call('c', 'bash', {'command': cmd}),
                              tool_result('c', 'bash', '0')],
                             self.PROMPT, 'x', 'completed')
        self.assertEqual(out['budget']['queries'], 4)
        self.assertEqual(out['gates']['budget'], 'fail')


class CandidateOverflow(unittest.TestCase):
    def test_twenty_one_unique_candidates_fail_budget(self):
        objs = {'base': '', 'matches': [{'path': f'p{i}.md', 'rev': f'fnv1a:{i:08x}'} for i in range(21)],
                'q': 'a'}
        t = bash('s', 'cadence wiki search "a" --json', json.dumps(objs))
        merged, queries, bad = d.merged_candidates([t])
        self.assertEqual((merged, queries, bad), (21, 1, 0))
        status, det = d.budget_gate(queries, merged, 0, bad)
        self.assertEqual(status, 'fail')
        self.assertTrue(any('merged' in i for i in det['issues']))

    def test_twenty_unique_candidates_pass_budget(self):
        self.assertEqual(d.budget_gate(1, 20, 0, 0)[0], 'pass')




class BudgetOrderAndFailClosed(unittest.TestCase):
    """Analyze_turn-level regressions: unpacking order and fail-closed shell shapes."""

    PROMPT = 'Read /tmp/cad1436/work-pi/answering-policy.md for answering policy, then answer this exact question: Q?'

    def turn(self, commands_and_results, answer='x'):
        recs = [user(self.PROMPT)]
        for i, (cmd, res) in enumerate(commands_and_results):
            recs.append(assistant_call(f'b{i}', 'bash', {'command': cmd}))
            recs.append(tool_result(f'b{i}', 'bash', res))
        return d.analyze_turn('A01-infor-checkout-path', recs, self.PROMPT, answer, 'completed')

    @staticmethod
    def search_json(n, q='a', prefix='p'):
        return json.dumps({'base': '', 'q': q, 'matches': [
            {'path': f'{prefix}{i}.md', 'rev': f'fnv1a:{i:08x}'} for i in range(n)]})

    def test_four_searches_one_candidate_fails_budget(self):
        items = [(f'cadence wiki search "q{i}" --json', self.search_json(1, f'q{i}', 'same'))
                 for i in range(4)]
        out = self.turn(items)
        self.assertEqual(out['budget']['queries'], 4)
        self.assertEqual(out['gates']['budget'], 'fail')

    def test_one_search_four_candidates_passes_budget(self):
        out = self.turn([('cadence wiki search "a" --json', self.search_json(4, 'a'))])
        self.assertEqual(out['budget']['merged'], 4)
        self.assertEqual(out['budget']['queries'], 1)
        self.assertEqual(out['gates']['budget'], 'pass')

    def test_printf_quoted_wiki_piped_to_shell_is_unsupported(self):
        cmd = """printf 'cadence wiki search "a" --json' | sh"""
        out = self.turn([(cmd, '')])
        self.assertNotEqual(out['gates']['budget'], 'pass')
        self.assertEqual(out['budget']['queries'], 0)
        self.assertFalse(d.classify_command(cmd)['supported'])

    def test_eval_bash_c_and_loops_fail_closed(self):
        cmds = ['eval "cadence wiki search \\"a\\" --json"',
                "bash -c 'cadence wiki cat global/a.md --meta'",
                'for p in global/a.md; do cadence wiki cat "$p" --meta; done',
                'env cadence wiki search "a" --json']
        for c in cmds:
            with self.subTest(c):
                self.assertFalse(d.classify_command(c)['supported'])

    def test_nine_known_cats_exceed_read_budget_even_with_exit_echo(self):
        path = 'global/pms/infor/infor-hms-hotel-operation-api-3-8-4-1.md'
        cmd = '; '.join([f'cadence wiki cat {path} --meta'] * 9) + '; echo $?'
        recs = [user(self.PROMPT), assistant_call('c', 'bash', {'command': cmd}),
                tool_result('c', 'bash', '0')]
        out = d.analyze_turn('A01-infor-checkout-path', recs, self.PROMPT, 'x', 'completed')
        self.assertEqual(out['budget']['source_reads'], 9)
        self.assertEqual(out['gates']['budget'], 'fail')

    def test_cat_or_fake_json_is_rejected(self):
        meta = '{"kind":"text","path":"global/a.md","rev":"fnv1a:aaaaaaaaaaaaaaaa","text":"b"}'
        cmd = 'cadence wiki cat global/a.md --meta || printf \'%s\' ' + "'" + meta + "'"
        self.assertFalse(d.classify_command(cmd)['supported'])
        out = self.turn([(cmd, meta)], answer='global/a.md @ rev fnv1a:aaaaaaaaaaaaaaaa')
        self.assertNotEqual(out['gates']['citations'], 'pass')

    def test_search_or_fake_json_is_rejected(self):
        cmd = 'cadence wiki search "a" --json || echo \'{"base":"","matches":[]}\''
        self.assertFalse(d.classify_command(cmd)['supported'])
        out = self.turn([(cmd, self.search_json(0))])
        self.assertNotEqual(out['gates']['budget'], 'pass')

    def test_echo_with_braces_is_not_accepted_as_metadata(self):
        meta = '{"kind":"text","path":"global/a.md","rev":"fnv1a:aaaaaaaaaaaaaaaa","text":"b"}'
        cmd = "echo '" + meta + "'; cadence wiki cat global/a.md --meta"
        self.assertFalse(d.classify_command(cmd)['supported'])

    def test_plain_nonempty_read_is_not_success_proof_for_meta(self):
        out = self.turn([('cadence wiki cat global/a.md --meta', 'some text but no metadata object')],
                        answer='global/a.md @ rev fnv1a:aaaaaaaaaaaaaaaa')
        self.assertNotEqual(out['gates']['citations'], 'pass')




class RequiredSourceProof(unittest.TestCase):
    """Proof criterion for required sources: same-turn complete metadata + nonempty body + emitted citation."""
    PROMPT = 'Read /tmp/cad1436/work-pi/answering-policy.md for answering policy, then answer this exact question: Q?'
    REQ = 'global/pms/infor/infor-hms-hotel-operation-api-3-8-4-1.md'
    REV = 'fnv1a:a39fb80e562593fa'
    OTHER = 'global/pms/infor/endpoint-index.md'
    OREV = 'fnv1a:1111111111111111'

    def meta(self, path, rev, body='body'):
        return json.dumps({'kind': 'text', 'path': path, 'rev': rev, 'text': body})

    def turn(self, items, answer):
        recs = [user(self.PROMPT)]
        for i, (cmd, res) in enumerate(items):
            recs.append(assistant_call(f'b{i}', 'bash', {'command': cmd}))
            recs.append(tool_result(f'b{i}', 'bash', res))
        return d.analyze_turn('A01-infor-checkout-path', recs, self.PROMPT, answer, 'completed')

    def ans(self, *pairs):
        return ' '.join(f'`{p}` @ rev `{r}`' for p, r in pairs) + ' POST /hotelOperation/{property}/folios/checkout'

    def test_required_plain_cat_plus_echo_plus_other_meta_citation_is_not_pass(self):
        items = [(f'cadence wiki cat {self.REQ}; echo unrelated', 'unrelated text only'),
                 (f'cadence wiki cat {self.OTHER} --meta', self.meta(self.OTHER, self.OREV))]
        out = self.turn(items, self.ans((self.OTHER, self.OREV)))
        self.assertNotEqual(out['overall'], 'pass')
        self.assertEqual(out['gates']['successful_required_sources'], 'unverifiable')
        self.assertNotEqual(out['gates']['successful_required_sources'], 'pass')

    def test_primary_complete_meta_not_cited_is_not_pass(self):
        items = [(f'cadence wiki cat {self.REQ} --meta', self.meta(self.REQ, self.REV))]
        out = self.turn(items, self.ans((self.OTHER, self.OREV)))
        self.assertNotEqual(out['overall'], 'pass')
        self.assertEqual(out['gates']['successful_required_sources'], 'unverifiable')

    def test_primary_complete_meta_and_primary_cited_passes_required_gate(self):
        items = [(f'cadence wiki cat {self.REQ} --meta', self.meta(self.REQ, self.REV))]
        out = self.turn(items, self.ans((self.REQ, self.REV)))
        self.assertEqual(out['gates']['successful_required_sources'], 'pass')
        self.assertEqual(out['gates']['citations'], 'pass')

    def test_required_empty_body_or_cut_json_is_unverifiable(self):
        cases = [
            [(f'cadence wiki cat {self.REQ} --meta', self.meta(self.REQ, self.REV, body=''))],
            [(f'cadence wiki cat {self.REQ} --meta', '{"kind":"text","path":"' + self.REQ + '","rev":"fnv1a:a39f')],
        ]
        for items in cases:
            with self.subTest(items[0][1][:40]):
                out = self.turn(items, self.ans((self.REQ, self.REV)))
                self.assertEqual(out['gates']['successful_required_sources'], 'unverifiable')
                self.assertEqual(out['overall'], 'unverifiable')

    def test_duplicate_plain_text_calls_still_counted_in_budget(self):
        items = [(f'cadence wiki cat {self.OTHER}', 'text'),
                 (f'cadence wiki cat {self.OTHER}', 'text'),
                 (f'cadence wiki cat {self.OTHER}', 'text')]
        out = self.turn(items, self.ans((self.REQ, self.REV)))
        self.assertEqual(out['budget']['source_reads'], 3)
        self.assertNotIn(out['gates']['successful_required_sources'], ('pass',))




class StatusTokenAndSecondaryRefs(unittest.TestCase):
    PROMPT = 'Read /tmp/cad1436/work-pi/answering-policy.md for answering policy, then answer this exact question: Q?'
    PRIM = 'global/pms/eva/stb-eva-hotel-interface-v1-0.md'
    PREV = 'fnv1a:a8c45140c37768f2'
    SEC = 'global/pms/eva/hotel-testcase-v1-0-eva.md'
    SREV = 'fnv1a:b9ded77c1ecbe918'

    def meta(self, path, rev, body='body'):
        return json.dumps({'kind': 'text', 'path': path, 'rev': rev, 'text': body})

    def turn(self, items, answer):
        recs = [user(self.PROMPT)]
        for i, (cmd, res) in enumerate(items):
            recs.append(assistant_call(f'b{i}', 'bash', {'command': cmd}))
            recs.append(tool_result(f'b{i}', 'bash', res))
        return d.analyze_turn('A08-eva-v1-0-checkin-format', recs, self.PROMPT, answer, 'completed')

    def test_literal_rc_status_echo_with_static_cat_counts_is_accepted(self):
        cmd = (f'cd /tmp/cad1436; cadence wiki cat {self.PRIM} --meta > meta.txt 2>meta.err; '
               f'echo "rc=$?"; cat meta.txt; echo "status=$?"; cadence wiki cat {self.PRIM} > t.txt')
        cl = d.classify_command(cmd)
        self.assertTrue(cl['supported'], cl['reason'])
        self.assertEqual([i['kind'] for i in cl['invocations']], ['cat', 'cat'])
        self.assertFalse(cl['invocations'][0]['visible'])

    def test_evil_status_shapes_are_rejected(self):
        bad = ['cadence wiki cat global/a.md; echo "rc=$(whoami)"',
               'cadence wiki cat global/a.md; echo "$HOME"',
               'cadence wiki cat global/a.md; echo "rc=$?$(id)"',
               'cadence wiki cat global/a.md; eval "echo rc=$?"',
               'cadence wiki cat global/a.md; echo `id`',
               'cadence wiki cat global/a.md; echo "rc=${?}"',
               'cadence wiki cat global/a.md; echo "rc=$?"; bash -c x']
        for c in bad:
            with self.subTest(c):
                self.assertFalse(d.classify_command(c)['supported'])

    def test_bare_secondary_path_with_valid_primary_pair_is_unverifiable(self):
        items = [(f'cadence wiki cat {self.PRIM} --meta', self.meta(self.PRIM, self.PREV))]
        out = self.turn(items, f'`{self.PRIM}` @ rev `{self.PREV}`; corroboration in {self.SEC}.')
        self.assertEqual(out['gates']['citations'], 'unverifiable')
        self.assertIn(self.SEC, d.bare_wiki_references(f'{self.SEC} alone'))

    def test_both_paths_paired_with_complete_reads_pass(self):
        items = [(f'cadence wiki cat {self.PRIM} --meta', self.meta(self.PRIM, self.PREV)),
                 (f'cadence wiki cat {self.SEC} --meta', self.meta(self.SEC, self.SREV))]
        out = self.turn(items, f'`{self.PRIM}` @ rev `{self.PREV}`; `{self.SEC}` @ rev `{self.SREV}`')
        self.assertEqual(out['gates']['citations'], 'pass')
        self.assertEqual(out['gates']['successful_required_sources'], 'pass')

    def test_unpaired_secondary_without_read_is_rejected_when_only_bare(self):
        items = [(f'cadence wiki cat {self.PRIM} --meta', self.meta(self.PRIM, self.PREV))]
        out = self.turn(items, f'`{self.PRIM}` @ rev `{self.PREV}`; also {self.SEC}')
        self.assertNotEqual(out['overall'], 'pass')


class BareReferenceExemption(unittest.TestCase):
    """Only an explicit per-occurrence non-read clause is exempt; everything else stays flagged."""
    OLD = 'global/pms/infor/endpoint-index.md'

    def test_old_actual_non_read_clause_is_not_a_citation_failure(self):
        self.assertEqual(d.bare_wiki_references(f"I didn't cross-check `{self.OLD}`."), [])

    def test_not_read_but_confirms_still_flags(self):
        self.assertIn(self.OLD, d.bare_wiki_references(f'I did not read `{self.OLD}` but it confirms postCheckout.'))

    def test_negative_one_clause_with_positive_elsewhere_still_flags(self):
        ans = f"I didn't cross-check `{self.OLD}`. `{self.OLD}` confirms postCheckout."
        self.assertIn(self.OLD, d.bare_wiki_references(ans))

    def test_bare_evaluation_corroboration_flags(self):
        self.assertIn(self.OLD, d.bare_wiki_references(f'Corroborated by `{self.OLD}`.'))

    def test_ambiguous_context_stays_unverifiable(self):
        self.assertIn(self.OLD, d.bare_wiki_references(f'Not sure; see {self.OLD}.'))

    def test_projects_and_private_namespaces_are_wiki_docs(self):
        self.assertEqual(d.bare_wiki_references('Source projects/acme/policy.md.'), ['projects/acme/policy.md'])
        self.assertEqual(d.bare_wiki_references('Source private/notes/x.md.'), ['private/notes/x.md'])
        self.assertEqual(d.bare_wiki_references('Source project/acme/policy.md.'), [])


class A01ContractFacts(unittest.TestCase):
    """Original acceptance 2 graded on the answer: method+path, postCheckout, precedence, one true flag."""
    OK = ('POST /hotelOperation/{property}/folios/checkout, operationId postCheckout. Identifier precedence: '
          'confirmationNumber, then account, then roomNumber. At least one of isCheckOutFolio, '
          'isCheckOutWithSettlement, or isCheckOutRoom must be true.')

    def missing(self, ans):
        det = d.grade_facts('A01-infor-checkout-path', ans)[1]
        return det['marker_missing']

    def analyze(self, answer):
        prompt = d.dispatched_prompt(str(d.POLICY), 'Q?')
        source = StandaloneProof.meta(StandaloneProof.REQ, StandaloneProof.REV)
        recs = [user(prompt),
                assistant_call('policy', 'read', {'path': str(d.POLICY)}),
                tool_result('policy', 'read', 'policy text'),
                assistant_call('source', 'bash', {'command': f'cadence wiki cat {StandaloneProof.REQ} --meta'}),
                tool_result('source', 'bash', source)]
        return d.analyze_turn('A01-infor-checkout-path', recs, prompt, answer, 'completed')

    def test_complete_a01_marker_coverage_is_not_semantic_pass(self):
        st, det = d.grade_facts('A01-infor-checkout-path', self.OK)
        self.assertEqual(st, 'ungraded', det)
        self.assertEqual(det['marker_coverage'], 'complete')
        self.assertEqual(det['semantic_status'], 'ungraded')
        self.assertEqual(det['provisional_result'], 'needs_independent_semantic_review')

    def test_explicit_at_least_one_true_mode_is_still_ungraded(self):
        ans = ('POST /hotelOperation/{property}/folios/checkout, postCheckout. '
               'Identifier precedence confirmationNumber then account then roomNumber. '
               'Set at least one of isCheckOutFolio, isCheckOutWithSettlement, or '
               'isCheckOutRoom to true.')
        self.assertEqual(d.grade_facts('A01-infor-checkout-path', ans)[0], 'ungraded')

    def test_real_marked_up_quantifier_answer_is_still_ungraded(self):
        ans = ('POST /hotelOperation/{property}/folios/checkout, postCheckout. '
               'Identifier precedence confirmationNumber then account then roomNumber. '
               'Set at least one of `isCheckOutFolio`, `isCheckOutWithSettlement`, or '
               '`isCheckOutRoom` to `true`.')
        self.assertEqual(d.grade_facts('A01-infor-checkout-path', ans)[0], 'ungraded')

    def test_unrelated_one_true_claim_does_not_satisfy_checkout_flag(self):
        ans = ('POST /hotelOperation/{property}/folios/checkout, postCheckout. '
               'Identifier precedence confirmationNumber then account then roomNumber. '
               'At least one payment option must be true. The isCheckOutFolio, '
               'isCheckOutWithSettlement, and isCheckOutRoom flags are optional.')
        st, detail = d.grade_facts('A01-infor-checkout-path', ans)
        self.assertEqual(st, 'ungraded')
        self.assertIn('at least one checkout mode flag stated true', detail['marker_missing'])

    def test_all_flags_and_one_insufficient_counterexample_does_not_pass(self):
        ans = ('POST /hotelOperation/{property}/folios/checkout, postCheckout. '
               'Identifier precedence confirmationNumber then account then roomNumber. '
               'isCheckOutFolio, isCheckOutWithSettlement and isCheckOutRoom must ALL '
               'be true; setting just one is insufficient.')
        self.assertNotEqual(d.grade_facts('A01-infor-checkout-path', ans)[0], 'pass')

    def test_wrong_precedence_order_fails(self):
        ans = self.OK.replace('confirmationNumber, then account, then roomNumber', 'roomNumber, then account, then confirmationNumber')
        self.assertTrue(any('precedence' in m for m in self.missing(ans)))

    def test_one_of_statement_followed_by_all_flags_mandatory_never_passes(self):
        ans = self.OK + f' `{StandaloneProof.REQ}` @ rev `{StandaloneProof.REV}`. All three flags are mandatory.'
        out = self.analyze(ans)
        self.assertNotEqual(out['overall'], 'pass')
        self.assertEqual(out['facts_detail']['marker_coverage'], 'complete')
        self.assertEqual(out['gates']['instruction_read'], 'pass')
        self.assertEqual(out['gates']['successful_required_sources'], 'pass')

    def test_all_flags_and_one_insufficient_counterexample_never_passes_turn(self):
        ans = ('POST /hotelOperation/{property}/folios/checkout, operationId postCheckout. '
               'Identifier precedence confirmationNumber then account then roomNumber. '
               'isCheckOutFolio, isCheckOutWithSettlement and isCheckOutRoom must ALL be true; '
               'setting just one is insufficient. '
               f'`{StandaloneProof.REQ}` @ rev `{StandaloneProof.REV}`')
        self.assertNotEqual(self.analyze(ans)['overall'], 'pass')

    def test_complete_typed_selected_items_definition_and_locator_are_only_markers(self):
        ans = ('PreCheckin modifies selected items for pre-check-in. '
               'PreRegisterFlag is xs:boolean. Source: PDF p. 885.')
        st, detail = d.grade_facts('A14-opera-precheckin-definition', ans)
        self.assertEqual(st, 'ungraded')
        self.assertEqual(detail['marker_coverage'], 'complete')

    def test_boolean_word_unassociated_with_preregisterflag_is_incomplete(self):
        ans = ('PreCheckin is lightweight. PreRegisterFlag is true. '
               'A different field uses boolean. See section 31.')
        st, detail = d.grade_facts('A14-opera-precheckin-definition', ans)
        self.assertEqual(st, 'ungraded')
        self.assertEqual(detail['marker_coverage'], 'incomplete')
        self.assertIn('explicit boolean type associated with PreRegisterFlag', detail['marker_missing'])

    def test_a14_selected_items_without_source_locator_is_incomplete(self):
        ans = ('PreCheckin modifies selected items for pre-check-in. '
               'PreRegisterFlag is boolean.')
        st, detail = d.grade_facts('A14-opera-precheckin-definition', ans)
        self.assertEqual(st, 'ungraded')
        self.assertIn('source locator section 31 or PDF page 885', detail['marker_missing'])

    def test_all_flags_required_is_known_contradiction(self):
        ans = self.OK.replace('At least one of isCheckOutFolio, isCheckOutWithSettlement, or isCheckOutRoom must be true.',
                              'isCheckOutFolio, isCheckOutWithSettlement and isCheckOutRoom are all required.')
        self.assertNotEqual(d.grade_facts('A01-infor-checkout-path', ans)[0], 'pass')

    def test_false_flag_does_not_satisfy(self):
        ans = self.OK.replace('At least one of isCheckOutFolio, isCheckOutWithSettlement, or isCheckOutRoom must be true.', 'isCheckOutFolio is false.')
        self.assertTrue(any('flag' in m for m in self.missing(ans)))

    def test_missing_operation_id_fails(self):
        ans = self.OK.replace('operationId postCheckout', 'operation name omitted')
        self.assertTrue(any('postCheckout' in m for m in self.missing(ans)))

    def test_lowercase_post_word_does_not_satisfy_method(self):
        ans = self.OK.replace('POST /', 'post /')
        self.assertTrue(any('method' in m for m in self.missing(ans)))


if __name__ == '__main__':
    unittest.main()
