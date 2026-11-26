import assert from 'node:assert/strict'
import {readFileSync} from 'node:fs'
import {test} from 'node:test'

test('no account slots, client login IPC, poller or account quota copy in the shipped graph', () => {
  const read = file => readFileSync(new URL('../ui/dist/'+file, import.meta.url), 'utf8')
  const graph = JSON.parse(read('client-graph.json'))
  const ids = ['@xharness/dsh-client-ui-settings-models', '@xharness/dsh-client-ui-settings-general', '@xharness/dsh-client-ui-model-selection']
  for (const id of ids) {
    assert.ok(graph.entries.some(entry => entry.id === id), `missing active module ${id}`)
    const js = read(`plugins/${id}/client.js`)
    assert.doesNotMatch(js, /desktop_account_|xharness-account-ready|xharness-managed-account|Account & allowance|accountReadOnly|source\.account/)
  }
  assert.match(read('plugins/@xharness/dsh-client-ui-settings-general/client.js'), /data-xh-settings-trigger/)
})
