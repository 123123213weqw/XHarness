// Settled-node identity prevents stream-delta updates from rerendering this row.
// Mounted on 'conversation.composer.status' so it rides with the composer in the
// active conversation scrollport (see ConversationRoot data-conversation-scroll).

import { throughput } from '../../shared/reception-metrics'
import { memo, useId, useMemo } from 'react'
import type { ConversationSnapshot, UseProjection } from "../types/runtime"
import type { SnapshotSelectorHook } from "../types/slots"
import type { ContextComposition, ContextPressureProjection, TokenUsageProjection } from "../types/wire"
import type { ComposerBarProps } from '../contract/slots'
import { formatTokensPerSecond } from './message-chrome'
import { assistantStepReading } from './turn-metrics'
import css from './StatsLine.styles'

interface WindowStats {
  turns: number
  steps: number
  /** Summed request wall time (step/start → assistant/message); 0 when no node carries timing. */
  llmMs: number
  /** Summed tool wall time (tool/call → tool/result); 0 when no pair is in-window. */
  toolMs: number
  /** Summed first-token latency over `ttftSteps`; 0 when no step records it. */
  ttftMs: number
  /** Steps carrying a recorded TTFT. */
  ttftSteps: number
  /** Summed decode wall time over steps that also report output tokens. */
  decodeMs: number
  /** Summed output tokens over the same decode-timed steps. */
  decodeTokens: number
}

/**
 * Fold assistant and tool-result nodes into window-scoped display totals —
 * the FALLBACK for assemblies without the `sessionStats` projection.
 *
 * Every displayed figure rides that durable whole-log projection (and token
 * accounting rides `tokenUsage`) because the window is paged and compaction
 * rewrites it; this fold answers "what is on screen" only when no projection
 * value is served. Its field names deliberately mirror the projection's so
 * the two swap wholesale.
 * @param nodes - snapshot nodes.
 * @returns fallback counts and summed wall times.
 */
export function deriveStats(nodes: ConversationSnapshot['nodes']): WindowStats {
  const turns = new Set<number>()
  let steps = 0
  let llmMs = 0
  let toolMs = 0
  let ttftMs = 0
  let ttftSteps = 0
  let decodeMs = 0
  let decodeTokens = 0
  for (const node of nodes) {
    if (node.kind === 'tool-result') {
      if (node.callTime !== null) toolMs += Math.max(0, node.time - node.callTime)
      continue
    }
    if (node.kind !== 'assistant') continue
    turns.add(node.turn)
    steps += 1
    if (node.timing !== undefined && node.timing.stepStartTime !== null) {
      llmMs += Math.max(0, node.timing.completedTime - node.timing.stepStartTime)
    }
    const reading = assistantStepReading(node)
    if (reading.ttftMs !== null) {
      ttftMs += reading.ttftMs
      ttftSteps += 1
    }
    if (reading.decodeMs !== null && reading.outputTokens !== null) {
      decodeMs += reading.decodeMs
      decodeTokens += reading.outputTokens
    }
  }
  return { turns: turns.size, steps, llmMs, toolMs, ttftMs, ttftSteps, decodeMs, decodeTokens }
}

/**
 * Compact token count: 517 / 12.2K / 517K / 1.2M (one decimal under three digits).
 * @param n - token count.
 * @returns display string.
 */
export function formatTokens(n: number): string {
  const scaled = (v: number): string =>
    v >= 100 ? String(Math.round(v)) : String(Math.round(v * 10) / 10)
  if (n < 1_000) return String(n)
  if (n < 1_000_000) return `${scaled(n / 1_000)}K`
  return `${scaled(n / 1_000_000)}M`
}

/**
 * Compact duration: 45.2s under a minute, 2m42s from there on.
 * @param ms - duration in milliseconds.
 * @returns display string.
 */
export function formatDuration(ms: number): string {
  const s = ms / 1_000
  if (s < 60) return `${Math.round(s * 10) / 10}s`
  const whole = Math.round(s)
  return `${Math.floor(whole / 60)}m${whole % 60}s`
}

/**
 * Cache-hit share of prompt-side input over the whole durable log.
 * @param usage - the session's token-usage projection value.
 * @returns rounded integer percent, or null when no input was billed.
 */
export function cacheHitPercent(usage: TokenUsageProjection): number | null {
  const denominator = billedInputTokens(usage)
  return denominator === 0
    ? null
    : Math.round(usage.cacheReadTokens / denominator * 100)
}

/**
 * Sum the three disjoint prompt-side billing buckets.
 * @param usage - the session's token-usage projection value.
 * @returns billed input tokens.
 */
export function billedInputTokens(usage: TokenUsageProjection): number {
  return usage.uncachedInputTokens + usage.cacheReadTokens + usage.cacheWriteTokens
}

interface ContextOccupancy {
  percent: number
  usedTokens: number
  contextWindow: number
  exact: boolean
  label: string
  accuracy: string
  stale: boolean
  composition?: ContextComposition | undefined
}

/** The ring is a request-bound provider sample, never an admission byte ceiling.
 * Pending estimates remain separate so preparation/interruptions cannot reprice
 * an earlier sample or swap its composition with a different request's shares.
 */
export function contextOccupancy(
  pressure: ContextPressureProjection | undefined,
): ContextOccupancy | null {
  const current = pressure?.pressureTokens
  const measured = Number.isSafeInteger(current) && (current ?? -1) >= 0 && (pressure?.pressureAccuracy ?? 'provider_reported') === 'provider_reported'
  const sample = pressure?.lastMeasured ?? (measured ? { tokens: current, contextWindow: pressure?.contextWindow, composition: pressure?.composition } : undefined)
  const usedTokens = sample?.tokens, contextWindow = sample?.contextWindow
  if (usedTokens === undefined || !Number.isSafeInteger(usedTokens) || usedTokens < 0 || contextWindow === undefined || !Number.isSafeInteger(contextWindow) || contextWindow <= 0) return null
  // Keep the sample's own denominator even if route capacity changes meanwhile.
  const stale = !measured || pressure?.phase === 'history_changed'
  const label = '最近请求实际输入' + (stale ? ' · 待更新' : '')
  return { percent: Math.min(100, Math.round(usedTokens / contextWindow * 100)), usedTokens, contextWindow, exact: true, label, accuracy: 'provider_reported', stale, composition: sample?.composition }
}

/** One readout on the strip: a muted label, a brighter value, an optional muted unit. */
interface StatMetric {
  key: string
  label: string
  /** Native tooltip: the precise meaning a short label cannot carry. */
  hint: string
  value: string
  unit?: string | undefined
}

/** Props: the conversation-snapshot selector plus the projection read seat. */
export interface StatsLineProps {
  useSession: SnapshotSelectorHook<ConversationSnapshot>
  useProjection: UseProjection
  /** The owning dock's locale seat. */
  t: ComposerBarProps['t']
}

export const StatsLine = memo(function StatsLine({ useSession, useProjection, t }: StatsLineProps) {
  const panelId = useId()
  const settledNodes = useSession(s => s.chat.legacy.nodes)
  const usage = useProjection('tokenUsage')
  // Every figure rides the durable sessionStats projection, so paging and
  // compaction cannot change any of them; an assembly without the unit falls
  // back to the window-scoped fold wholesale (same field names), paid only
  // while no projection value is served.
  const projected = useProjection('sessionStats')
  const stats = useMemo(() => projected ?? deriveStats(settledNodes), [projected, settledNodes])
  // Metrics with no data drop out whole; all reported metrics remain available.
  // The readout leads with what the user watches while the model streams,
  // then reports what the session has accumulated.
  const live: StatMetric[] = []
  const session: StatMetric[] = []
  const rateTotals = projected?.activeThroughput ?? stats
  const rate = throughput(rateTotals.decodeTokens, rateTotals.decodeMs)
  if (stats.steps > 0) {
    if (stats.ttftSteps > 0) {
      live.push({ key: 'ttft', label: t('stats.ttft'), hint: t('stats.ttftHint'), value: formatDuration(stats.ttftMs / stats.ttftSteps) })
    }
    if (rate !== null) {
      live.push({ key: 'speed', label: t('stats.speed'), hint: t('stats.speedHint'), value: formatTokensPerSecond(rate), unit: t('stats.tokensPerSecond') })
    }
    if (stats.llmMs > 0) session.push({ key: 'llm', label: t('stats.llm'), hint: t('stats.llmHint'), value: formatDuration(stats.llmMs) })
    if (stats.toolMs > 0) session.push({ key: 'tools', label: t('stats.toolCall'), hint: t('stats.toolCallHint'), value: formatDuration(stats.toolMs) })
    session.push(
      { key: 'turns', label: t('stats.turns'), hint: t('stats.turnsHint'), value: String(stats.turns) },
      { key: 'steps', label: t('stats.steps'), hint: t('stats.stepsHint'), value: String(stats.steps) },
    )
  }
  // Context occupancy deliberately lives on the composer's ContextMeter ring,
  // not here — one home per fact.
  // Billing rides the durable projection, so these survive paging and
  // compaction. Gated on actual token activity: a session whose steps all
  // settled without billing (e.g. every request failed) shows its counts
  // without a zero-token group.
  if (usage !== undefined
    && (billedInputTokens(usage) > 0 || usage.outputTokens > 0)) {
    const cacheHit = cacheHitPercent(usage)
    if (cacheHit !== null) live.push({ key: 'cache', label: t('stats.cacheHit'), hint: t('stats.cacheHitHint'), value: `${cacheHit}%` })
    live.push(
      { key: 'input', label: t('stats.inputTokens'), hint: t('stats.inputTokensHint'), value: formatTokens(billedInputTokens(usage)) },
      { key: 'output', label: t('stats.outputTokens'), hint: t('stats.outputTokensHint'), value: formatTokens(usage.outputTokens) },
    )
  }
  const rows = [live, session].filter(row => row.length > 0)
  if (rows.length === 0) return null
  // Persistent readout under the composer: a muted label, a brighter value.
  // These figures exist to be watched continuously, so they render inline,
  // always on. No disclosure interaction, no popup geometry. Live-region
  // chatter stays off because the values tick many times a second.
  return (
    <div className={css.root} data-composer-stats>
      <div id={panelId} className={css.strip} role="status" aria-live="off" aria-label={t('stats.title')}>
        {rows.map((row, index) => (
          <div key={index} className={css.row}>
            {row.map(metric => (
              <span key={metric.key} className={css.metric} title={metric.hint}>
                {metric.label}
                <span className={css.value}>{metric.value}</span>
                {metric.unit}
              </span>
            ))}
          </div>
        ))}
      </div>
    </div>
  )
})
