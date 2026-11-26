/** Regression for the former Assistant surface; keep the historical CI command ID. */
import assert from 'node:assert/strict'
import {existsSync, readFileSync} from 'node:fs'
import {test} from 'node:test'
import {assertProductionSources} from './ui-build-variant.mjs'

const source = (path) => readFileSync(new URL('../'+path, import.meta.url), 'utf8')

test('no retired Assistant source, production asset or registration remains', () => {
  assert.equal(existsSync(new URL('../ui/src/modules/assistant', import.meta.url)), false)
  assert.throws(() => assertProductionSources(['src/modules/assistant/index.tsx']), /Retired Little X/)
  const id = '@xlang/xharness-client-ui-assistant'
  for (const [path, list] of [['ui/modules.json','modules'], ['ui/dist/client-graph.json','entries']]) {
    const rows = JSON.parse(source(path))[list]
    assert.ok(Array.isArray(rows) && rows.length > 0)
    assert.equal(rows.some(row => row.id === id), false)
  }
  assert.equal(existsSync(new URL('../ui/dist/plugins/'+id, import.meta.url)), false)
  const frame = source('ui/src/modules/layout/AppFrame.tsx')
  assert.doesNotMatch(frame, /assistant.center|assistantVisible|xhassistant-seat/)
  assert.equal([...frame.matchAll(/renderSlot\('conversation', \{\}\)/g)].length, 1)
})

test('retired PR bindings remain fail-closed and no historical review hands off to an assistant', () => {
  const fixture = source('ui/src/modules/code-review/index.tsx') + source('ui/src/modules/code-review/ReviewPanel.tsx')
  assert.doesNotMatch(fixture, /openAssistant|pullReference|Ask Little X|github\/chat-(?:get|set)/)
  const native = source('crates/xharness-host/src/rpc.rs')
  assert.match(native, /github\/chat-get.*github\/chat-set/)
  assert.match(native, /PR chat binding has been retired; use an ordinary chat/)
})
