# ooze: design

15 October 2026. The screen was reviewed; the rest is the plan being built.

## What it is

ooze shows a Vercel team's projects live in a terminal, in the style of sweep and hauntop: who's on each site right now, what's deploying, what's failing, and what it all costs. It can cancel, redeploy, promote and roll back deployments, each after you confirm, and it notifies you when a production deploy fails or traffic spikes.

Its creature is badcoffee.dev's Slime, which is shaped like a triangle. A slime oozes the way haunt's ghost haunts.

## Decisions

- **Name and creature:** ooze and the Slime. The name is free on crates.io and Homebrew; most verbs for watching aren't.
- **How live:** Vercel's REST API, polled. Page views and visitors are about a minute behind and refresh every 10 seconds. A drain pushing each page view to a relay would be seconds behind, but it needs Pro, is billed by volume and needs something deployed in the team's account, so it's left for later.
- **What's in it** besides analytics, deployments and billing: error rates, cache hits and firewall blocks; a feed of Vercel's alerts, team activity and status-page incidents; AI Gateway spend; domains; notifications; actions. Speed Insights is left for later.
- **How it's built:** Rust, ratatui, threads and channels, like sweep and hauntop. macOS and Linux.

## Where the numbers come from

Checked on 15 October 2026 against Vercel's OpenAPI spec (`https://openapi.vercel.sh/`) and a Pro team.

| Number | Source | Refresh |
|---|---|---|
| Online now: unique visitors in the last 5 minutes | `POST /v2/observability/query`, metric `vercel.analytics_pageview.count`, aggregation `unique/visitor_id`, grouped by `project_id`, team scope | 10 s |
| Page views a minute, last hour | The same metric summed, in 1-minute buckets. The newest minute was under a minute old. | 10 s |
| Visitors and page views today | The same metric in one day's bucket, aligned to the local time zone with `bucketTimezone` | 5 min |
| 7 and 30 days; top pages, referrers and countries | `GET /v1/query/web-analytics/visits/count` and `/aggregate`. Counts round to whole UTC days, and `until` is required although the spec says it's optional. | when a project is opened |
| Requests, 5xx, cache hits | `vercel.request.count` grouped by `project_id`, filtered on `http_status ge 500`, and by `cache_result` | 30 s |
| Firewall | `vercel.firewall_action.count`; `GET /v1/security/firewall/attack-status` per project | 30 s; 2 min |
| AI Gateway | `vercel.ai_gateway_request.cost` and `.count`; the period's spend from billing | 5 min |
| Projects | `GET /v10/projects`: `webAnalytics.enabledAt`, `targets.production` | 5 min |
| Deployments | `GET /v7/deployments`, every project at once; build logs from `GET /v3/deployments/{id}/events?follow=1` while a deployment's details are open | 5 s while one builds, 15 s otherwise |
| Plan, period and credit | `GET /v2/teams/{id}`: `billing.plan`, `billing.period`, `billing.invoiceItems.includedAllocationUsd` (the included credit), seats, add-ons and spend controls | 10 min |
| Spend | `GET /v1/billing/charges`: FOCUS 1.3 as JSON lines, a row per day, service, project and region, about 1,500 rows a day on a small team. Days that have ended are cached on disk. | 30 min |
| Domains | `GET /v5/domains` (`expiresAt`, `renew`) and `GET /v6/domains/{domain}/config` (`misconfigured`) | hourly |
| Feed | `GET /v3/events` (team activity, with readable text); `GET /alerts/v3/groups` (Vercel's anomaly alerts, used by the Vercel CLI but not in the published spec, so ooze carries on without it); `https://www.vercel-status.com/api/v2/summary.json` | 1 to 2 min |

The rate limits seen were 100 observability queries a minute, 400 project requests and 1,000 deployment requests. Steady state uses about 25 observability queries a minute. ooze reads `x-ratelimit-remaining` and slows down when a fifth is left.

## The screen

```text
╭ ooze  night-shift · Pro ──────────────────────────────────── vercel ● api ● · updated 4s ago · 18:42 ╮
│ online 23   ▂▃▃▅▇▆▅▄▅▆▇█▆▅▃▄▅▆   today 1,204 visitors · 4,532 views   building 1   $25.31 of $20     │
╰──────────────────────────────────────────────────────────────────────────────────────────────────────╯
╭ projects · 3 with visitors ──────────────────────────────────────────────────────────────────────────╮
│   project        online  last 30 min       views/h   today   5xx    cache  production                │
│ ● moth-radio         18  ▂▃▅▇█▆▅▃▂▃▅▆▇█▆▅      312   3,410   0.1%   94%    ready · 2h ago            │
│ ● cave-notes          4  ▁▁▂▁▃▂▁▂▁▁▂▃▂▁▁▂       41     980   0%     99%    building · 1m 12s         │
│ ● night-market        1  ▁ ▁  ▁    ▁  ▁         6     142   0%     97%    failed · 4m ago            │
│ · 7 quiet projects · a shows them                                                                    │
╰──────────────────────────────────────────────────────────────────────────────────────────────────────╯
╭ deployments · 1 building ──────────────────────────────╮╭ billing · day 13 of 31 ────────────────────╮
│ ⠹ cave-notes    production  1m 12s  main "fix search"  ││ usage $25.31 of $20 credit ━━━━━━━━ 127%   │
│ ✗ night-market  production  build failed · 4m ago      ││ projected $58 by 3 Nov · over by $5.31     │
│ ✓ cave-notes    preview     38s  feat/tags             ││ data transfer $8.20 · fluid CPU $6.10 · …  │
│ ✓ moth-radio    production  current · 2h ago           ││ AI Gateway $3.20 · seats 0 · no add-ons    │
╰────────────────────────────────────────────────────────╯╰────────────────────────────────────────────╯
╭ feed ────────────────────────────────────────────────────────────────────────────────────────────────╮
│ 18:41 ✗ night-market  production build failed: npm run build exited with 1                           │
│ 18:40 ⠹ cave-notes    production build started · main a1b2c3d "fix search" · pat                     │
│ 18:30 ↑ moth-radio    traffic 4× its usual · 18 online, most from news.ycombinator.com               │
│ 17:55 ◆ cave-notes    pat changed the environment variable API_URL                                   │
╰─ tab panel  ↑↓ move  ⏎ details  o open  x cancel  R redeploy  P promote  B roll back  ? keys  q quit ╯
```

- **Header:** online now across all projects, page views a minute, today's visitors and views, builds running, and spend against the credit. Lights for Vercel's status page and the API. The border turns orange when something needs you (a failed production deploy, an error spike, an incident) and red when Vercel can't be reached.
- **Projects:** a row per project, busiest first: online, page views a minute for the last 30 minutes, views in the last hour, visitors today, 5xx rate, cache hit rate, and its production deployment. Projects without Web Analytics show requests instead. Quiet ones fold into one line; `a` opens it.
- **Deployments:** building and queued first, then the newest across all projects. A failed one says why.
- **Billing:** the period, usage against the included credit, the projection to the period's end, the costliest services, AI Gateway, seats and add-ons. The Slime sits beside it when the terminal is tall enough, glows when traffic jumps or a deploy lands, and sleeps when Vercel can't be reached.
- **Feed:** deploys starting, finishing and failing, traffic and error spikes, Vercel's alerts, team activity, status-page incidents, domain warnings, and what ooze did: notifications and actions.
- **⏎** opens details: a project's top pages, referrers, countries and errors; a deployment's build log, followed live while it builds, and the actions that apply.
- Below 120 columns billing folds into the header and the panels stack. `z` shows one panel on its own.

## Actions

| Key | Does | API |
|---|---|---|
| `x` | Cancels a building or queued deployment | `PATCH /v12/deployments/{id}/cancel` |
| `R` | Redeploys a deployment: the same commit, to the same environment | `POST /v13/deployments` with `deploymentId` |
| `P` | Promotes a ready deployment to production. A preview deployment is rebuilt with production's settings first; the dialog says which. | `POST /v10/projects/{id}/promote/{deploymentId}` |
| `B` | Rolls production back to an earlier production deployment. Vercel then stops assigning new production deployments until one is promoted; the dialog says so. | `POST /v1/projects/{id}/rollback/{deploymentId}` |

Each opens a dialog naming the project, the deployment and what will happen. `y` goes ahead; anything else doesn't. The outcome goes to the feed. `read_only = true` in the settings, or `--read-only`, turns them off.

## Notifications

- **Rules,** each a setting: a production deploy failed; a traffic spike (5 minutes of page views at least 3 times the last hour's 5-minute average, and at least 30); an error spike (5xx at least 5% of at least 50 requests in 5 minutes); an attack the firewall is mitigating; a Vercel incident; a domain expiring within 14 days without auto-renew; usage passing the included credit.
- **Delivery:** Notification Center on macOS (through `osascript`), `notify-send` on Linux, and the terminal's own notification (OSC 9) otherwise.
- **Who sends them:** the open view while it runs. `ooze watch` does the same without the interface, and `ooze agent install` starts it at login, as a LaunchAgent on macOS or a systemd user unit on Linux. Only the process holding the lock in ooze's cache folder notifies, so they never come twice.

## Signing in

1. `VERCEL_TOKEN`.
2. A token saved by `ooze login` in `~/.config/ooze/token`, readable only by you.
3. The Vercel CLI's login, its `auth.json`. That token lasts hours and is renewed with a refresh token that changes each time, so ooze never renews it itself: when it's about to expire, ooze runs `vercel whoami`, which renews it, and reads it again.

The team is `--team`, or `team` in the settings, or the Vercel CLI's current team, or the only one. `t` switches between them.

## When something fails

- **No token, or a rejected one:** the view says how to sign in, and `--once` exits with an error.
- **Billing refused** (a role without billing access): the billing panel says so and everything else carries on.
- **Rate limited:** ooze waits until `x-ratelimit-reset`, and the header's API light turns orange.
- **Vercel unreachable:** the Slime sleeps, the header's border turns red, the numbers stay with how old they are, and ooze tries again, waiting longer each time.
- **A project without Web Analytics** shows requests instead of page views.

## Code

`src/`: `main` (the commands), `theme`, `art` (the Slime, sparklines, meters), `fmt`, `config`, `auth`, `api` (the client and the response types), `poll` (a thread per source), `model` (the world, folding in each message), `billing`, `feed`, `notify`, `actions`, `tui`, `demo`, `report`, `agent`.

Tests cover parsing each response (from fixtures written by hand, nothing from a real account), billing sums and the projection, the spike rules, the order tokens are tried in and expiry, each action's preconditions, frames of the demo at several sizes, keys driving the view, and a local HTTP server standing in for Vercel for the client's retries and rate limits. `ooze demo` and `ooze snapshot` draw a made-up team for screenshots and tests.

## Release

MIT, at `github.com/pdcgomes/ooze`, installed with `cargo install --git https://github.com/pdcgomes/ooze --locked`, with CI on macOS and Linux. The README says ooze isn't affiliated with Vercel.

## Later

- A live feed of each page view, fed by a drain.
- Speed Insights.
- A map of where visitors are.
