// Reviewed single-scroll-viewport deltas for the tool sheets: the frozen UI
// reference is never rewritten; these exact forward patches reproduce the
// in-place token removals (bounded-height inner scrollers are gone — the
// conversation transcript is the only scrollport). Labels and headings that
// used to stick inside those scrollers lost their anchor: left sticky they
// floated over transcript text, so they flow with their card again.
const reviewedToolDeltas = {
  '@xharness/dsh-client-ui-tool/ToolRow.module.css': [
    ['.Dh215a_bodyScroll{max-height:260px;overflow-y:auto}', '.Dh215a_bodyScroll{}'],
    ['column-gap:14px;max-height:150px;padding:12px 16px;display:grid;overflow-y:auto}', 'column-gap:14px;padding:12px 16px;display:grid}'],
    ['.Dh215a_ioLabel{color:var(--dsw-alias-label-caption);align-self:start;position:sticky;top:0}', '.Dh215a_ioLabel{color:var(--dsw-alias-label-caption);align-self:start}'],
  ],
  '@xharness/dsh-client-ui-tool/bash-sample.module.css': [
    ['column-gap:14px;max-height:150px;padding:12px 16px;display:grid;overflow-y:auto}', 'column-gap:14px;padding:12px 16px;display:grid}'],
    ['.ANgngG_ioLabel{color:var(--dsw-alias-label-caption);align-self:start;position:sticky;top:0}', '.ANgngG_ioLabel{color:var(--dsw-alias-label-caption);align-self:start}'],
  ],
}

const reviewDiffDelta = [
  ['.xh-review-pane{min-width:0;max-height:400px;overflow:auto;scrollbar-gutter:stable;', '.xh-review-pane{min-width:0;'],
  ['.xh-review-pane-heading{position:sticky;top:0;z-index:1;display:block;', '.xh-review-pane-heading{display:block;'],
]

export function applyReviewedSingleViewportStyle(stylesheetId, text) {
  let deltas = reviewedToolDeltas[stylesheetId]
  if (deltas === undefined && (stylesheetId === undefined || stylesheetId === '') && text.includes('.xh-review-pane{')) {
    deltas = reviewDiffDelta
  }
  if (deltas === undefined) return text
  let out = text
  for (const [oldToken, newToken] of deltas) {
    const at = out.indexOf(oldToken)
    if (at < 0 || out.indexOf(oldToken, at + 1) >= 0) {
      throw Error('reviewed single-viewport delta anchor must appear exactly once: ' + oldToken.slice(0, 48))
    }
    out = out.slice(0, at) + newToken + out.slice(at + oldToken.length)
  }
  return out
}
