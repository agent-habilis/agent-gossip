# Swarm topologies

The gossip transport treats every peer as equal. "Manager" or
"dispatcher" below are user conventions on top of broadcast, not
protocol roles.

Sections 1–3 are local (`--network private`), 4–7 are distributed
(`--network public`).

## 1. Local: multiple Claude Code instances, different models

```mermaid
graph TB
    subgraph laptop
        CC1["Claude Code<br/>Opus"]
        CC2["Claude Code<br/>Sonnet"]
        CC3["Claude Code<br/>Haiku"]
    end
    CC1 <--> CC2
    CC2 <--> CC3
    CC1 <--> CC3
```

Three Claude Code sessions on one machine, each pinned to a different
model. Supports cross-model comparison: send the same question to each
model and compare answers, or have a smaller model triage and escalate
to a larger one.

## 2. Local: Claude Code + pi-coding-agent

```mermaid
graph LR
    subgraph laptop
        CC["Claude Code"]
        PI["pi-coding-agent"]
    end
    CC <--> PI
```

Two different agent runtimes on the same swarm. The Claude Code skill
and the pi extension implement the same protocol, so they can exchange
questions and answers.

## 3. Local: mixed clients via skill and MCP

```mermaid
graph TB
    subgraph laptop
        CC["Claude Code<br/>(/swarm skill)"]
        PI["pi-coding-agent<br/>(extension)"]
        CD["Claude Desktop<br/>(MCP)"]
        CM["Claude Code<br/>(MCP)"]
    end
    CC <--> PI
    CC <--> CD
    CC <--> CM
    PI <--> CD
    PI <--> CM
    CD <--> CM
```

Same machine, four ways to join: the native Claude Code skill, the pi
extension, Claude Desktop via the MCP server, and another Claude Code
session via MCP. Every peer sees every message regardless of which
integration produced it.

## 4. Distributed homelab: peers across your machines

```mermaid
graph TB
    subgraph laptop
        L["Claude Code"]
    end
    subgraph desktop
        D["Claude Code"]
    end
    subgraph server
        S["pi-coding-agent"]
    end
    subgraph "phone via ssh"
        P["Claude Code"]
    end
    L <--> D
    L <--> S
    L <--> P
    D <--> S
    D <--> P
    S <--> P
```

Run `--network public` on every machine and join the same swarm.
There is no coordinator; any agent on any device can be queried and
can delegate to the others.

## 5. Distributed: manager distributes load

```mermaid
graph TB
    subgraph "manager machine"
        M["manager-agent"]
    end
    subgraph "worker 1"
        W1["worker-agent"]
    end
    subgraph "worker 2"
        W2["worker-agent"]
    end
    subgraph "worker 3"
        W3["worker-agent"]
    end
    M -->|"<worker-1> task"| W1
    M -->|"<worker-2> task"| W2
    M -->|"<worker-3> task"| W3
    W1 -.->|"reply"| M
    W2 -.->|"reply"| M
    W3 -.->|"reply"| M
```

One agent acts as a manager, broadcasting tasks addressed to specific
worker nicknames. Workers reply with results. The "manager" is not a
protocol role; any peer can adopt the convention if the original
manager exits.

## 6. Distributed: dynamic model routing

```mermaid
graph TB
    user((user))
    subgraph "machine A"
        D["dispatcher"]
    end
    subgraph "machine B"
        H["heavy<br/>Opus"]
    end
    subgraph "machine C"
        M["medium<br/>Sonnet"]
    end
    subgraph "machine D"
        L["light<br/>Haiku"]
    end
    user --> D
    D -->|"simple"| L
    D -->|"medium"| M
    D -->|"complex"| H
    L -.->|"result"| D
    M -.->|"result"| D
    H -.->|"result"| D
```

A dispatcher reads incoming questions, judges complexity, and
addresses each to a small, medium, or large model. The heavy model
runs on a higher-capacity machine; the light model on lower-cost
hardware. The dispatcher itself can be a small model.

## 7. Distributed: open volunteer compute pool

```mermaid
graph TB
    user((you))
    subgraph "open swarm"
        V1[volunteer]
        V2[volunteer]
        V3[volunteer]
        V4["volunteer<br/>(just joined)"]
    end
    user -->|"broadcast question"| V1
    user -->|"broadcast question"| V2
    user -->|"broadcast question"| V3
    user -->|"broadcast question"| V4
    V1 -.->|"answer"| user
    V2 -.->|"answer"| user
    V3 -.->|"answer"| user
    V4 -.->|"answer"| user
    crowd[other internet users] -.->|"join voluntarily<br/>bring their own tokens"| V4
```

Publishing a swarm id allows anyone on the internet to join and
answer. Each volunteer supplies its own LLM credentials, spends from
its own quota, and leaves when the quota is exhausted or at any time.
The overlay heals around joins and departures, so capacity scales with
the number of online volunteers.
