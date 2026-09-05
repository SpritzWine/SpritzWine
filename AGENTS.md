# Obsidian Second Brain - OpenCode Operating Manual

This vault runs the **obsidian-second-brain** skill. The skill ships a set of
*commands*: each one is a multi-step instruction file that you (the OpenCode
agent) should follow when the user's request matches its trigger phrase.

## Mandatory rules (all projects)

Identical in every SpritzWine, Prater, beisl and gamebus repo and vault. Change
them everywhere or nowhere. They apply to everything an agent writes: code,
comments, commit messages, docs, notes.

1. Attribution: the only AI trailer is
   `Assisted-By: Claude <model> <noreply@anthropic.com>`, one line, last.
   Never `Co-Authored-By`. Never a `Claude-Session:` line or any session or
   tracking URL. Commits that only package the maintainer's own edits carry no
   AI trailer at all.
2. Plain ASCII punctuation: no em, en or figure dashes, no Unicode minus or
   non-breaking hyphen, no non-breaking spaces, no zero-width or bidi
   characters. Write `-` or `--`, or restructure the sentence. Before
   committing, run the unicode-hygiene skill
   (`python3 ~/.claude/skills/unicode-hygiene/scripts/scan_unicode.py`) on
   what you touched: hidden characters fail the run, dashes are reported.
   Vendored third-party files (SDK headers, LICENSE texts) are left as they are.
3. Comments: concise and load-bearing. Say why, not what. No narration of
   obvious code, no essays, no restating the commit message. Match the
   surrounding density; when in doubt, fewer.

## How to operate

1. Read `_CLAUDE.md` in the vault root, if it exists, to learn the user's
   vault conventions.
2. When the user's request matches a trigger in the tables below, read the
   matching file under `.opencode/commands/<name>.md` and follow its
   instructions step by step.
3. Treat the AI-first vault rule as non-negotiable for every note you write:
   `## For future Claude` preamble, rich frontmatter (`type`, `date`,
   `tags`, `ai-first: true`), `[[wikilinks]]` for every
   person/project/concept, recency markers per external claim, sources
   verbatim, confidence levels where applicable. The full spec is
   `.opencode/references/ai-first-rules.md`, a path relative to the install root. If it does not
   resolve from your working directory, search upward for it. If you still
   cannot read it, say so before writing - the seven requirements above still
   apply, and a note written while the spec was unreachable must never look
   the same as one written with it in hand.

## Command routing tables (grouped by category)

### Vault - daily writing, capture, find

| Command | What it does | Read this file |
|---|---|---|
| `/obsidian-board-hygiene` | Bulk-triage a kanban board - surface stale items and archive, reschedule, or mark them done in one pass | `.opencode/commands/obsidian-board-hygiene.md` |
| `/obsidian-board` | Show or update a kanban board - flags overdue items, updates from conversation | `.opencode/commands/obsidian-board.md` |
| `/obsidian-capture` | Quick idea capture - zero friction, saves to your ideas folder and mentions in daily note | `.opencode/commands/obsidian-capture.md` |
| `/obsidian-catchup` | Review and process everything captured on the go from the Telegram journal bot - voice, text, images, PDFs, links - waiting in the catchup queue. You pull it when you are back at the laptop; nothing is processed autonomously. | `.opencode/commands/obsidian-catchup.md` |
| `/obsidian-daily` | Create or update today's daily note - pulls calendar events, overdue tasks, and conversation context | `.opencode/commands/obsidian-daily.md` |
| `/obsidian-find` | Smart vault search - returns results with context, not just filenames | `.opencode/commands/obsidian-find.md` |
| `/obsidian-log` | Log this work or dev session to the vault - infers project from context | `.opencode/commands/obsidian-log.md` |
| `/obsidian-person` | Create or update a person note from conversation context | `.opencode/commands/obsidian-person.md` |
| `/obsidian-project` | Create or update a project note - adds to board and daily note automatically | `.opencode/commands/obsidian-project.md` |
| `/obsidian-projects` | Live project status from git + local docs - infers all context from vault notes, no config required | `.opencode/commands/obsidian-projects.md` |
| `/obsidian-recap` | Summarize a time period from the vault - today, week, or month | `.opencode/commands/obsidian-recap.md` |
| `/obsidian-recurring` | Track a recurring obligation (payment, filing, ops) with a cadence and a computed next-due date | `.opencode/commands/obsidian-recurring.md` |
| `/obsidian-save` | Save everything worth keeping from this conversation to the vault | `.opencode/commands/obsidian-save.md` |
| `/obsidian-task` | Add a task to the right kanban board with inferred priority and due date | `.opencode/commands/obsidian-task.md` |
| `/obsidian-world` | Load your identity, values, priorities, and current state in one shot - with progressive context levels to avoid burning tokens | `.opencode/commands/obsidian-world.md` |

### Thinking - synthesis, decisions, learning, reviews

| Command | What it does | Read this file |
|---|---|---|
| `/idea-discovery` | Surface 3-5 next-direction candidates by reading ungraduated ideas, open project questions, and orphan research notes - what is worth working on next | `.opencode/commands/idea-discovery.md` |
| `/obsidian-brainstorm` | Multi-turn Socratic brainstorm - one question per turn until the idea converges, then a design note with named alternatives and one recommendation | `.opencode/commands/obsidian-brainstorm.md` |
| `/obsidian-challenge` | Red-team your current idea against your own vault history - finds contradictions, past failures, and flawed assumptions | `.opencode/commands/obsidian-challenge.md` |
| `/obsidian-connect` | Bridge two unrelated domains using your vault's link graph - forces creative friction to spark new ideas | `.opencode/commands/obsidian-connect.md` |
| `/obsidian-decide` | Record decisions - lightweight by default (logged to project notes), or a full ADR record with --formal | `.opencode/commands/obsidian-decide.md` |
| `/obsidian-distill` | Condense a long note or source into key claims, each tagged with provenance back to the exact source block it came from | `.opencode/commands/obsidian-distill.md` |
| `/obsidian-emerge` | Surface unnamed patterns from your recent notes - recurring themes, hidden connections, and conclusions you haven't explicitly stated | `.opencode/commands/obsidian-emerge.md` |
| `/obsidian-graduate` | Promote an idea fragment into a full project spec with tasks, board entries, and structure | `.opencode/commands/obsidian-graduate.md` |
| `/obsidian-learn` | Review vault learnings, prune stale ones, surface active patterns - the vault's lessons compound or expire | `.opencode/commands/obsidian-learn.md` |
| `/obsidian-panel` | Convene a panel of distinct perspectives on a decision - one independent verdict per lens, then a synthesis. A multi-persona complement to /obsidian-challenge | `.opencode/commands/obsidian-panel.md` |
| `/obsidian-reconcile` | Find and resolve contradictions in the vault - the vault maintains its own truth | `.opencode/commands/obsidian-reconcile.md` |
| `/obsidian-review` | Generate a structured weekly or monthly review note from vault history | `.opencode/commands/obsidian-review.md` |
| `/obsidian-synthesize` | Automatic synthesis - scans the vault for unnamed patterns and writes synthesis pages without being asked | `.opencode/commands/obsidian-synthesize.md` |
| `/vault-deep-synthesis` | Deep cross-reference of everything the vault knows about one topic - agreements, contradictions, stale claims, and coverage gaps. Pure vault, no network | `.opencode/commands/vault-deep-synthesis.md` |

### Research - bring external sources into the vault

| Command | What it does | Read this file |
|---|---|---|
| `/notebooklm` | Vault-first source-grounded research via Gemini File Search. One command, no browser. The grounded parallel to /research-deep (which is open-web via Perplexity). | `.opencode/commands/notebooklm.md` |
| `/obsidian-ingest` | Ingest a source into the vault - the vault rewrites itself around new knowledge. Every ingest updates entities, rewrites stale claims, synthesizes new concepts, and resolves contradictions. | `.opencode/commands/obsidian-ingest.md` |
| `/podcast` | Extract metadata, transcript, and summary from a podcast episode, saved as an AI-first note in the vault | `.opencode/commands/podcast.md` |
| `/research-deep` | Vault-first deep research - scans the vault, fills gaps (Perplexity + Grok when keyed, free key-less sources otherwise), synthesizes a delta, then propagates updates across people/projects/ideas via /obsidian-save | `.opencode/commands/research-deep.md` |
| `/research` | Web research with citations - Perplexity Sonar when an API key is set, free key-less sources (Wikipedia, HackerNews, arXiv, Reddit, and more) otherwise. Deep dossier with summary, facts, timeline, players, contrarian views, open questions | `.opencode/commands/research.md` |
| `/x-pulse` | Scan X for what's trending in a topic - themes, voices, hooks, and post ideas powered by Grok x_search | `.opencode/commands/x-pulse.md` |
| `/x-read` | Deep-read an X (Twitter) post via Grok x_search - verbatim post, thread, TL;DR, claims, reply sentiment, voices to watch | `.opencode/commands/x-read.md` |
| `/youtube` | Extract transcript, metadata, and top comments from a YouTube video - summarized via Gemini (free tier) or Grok and saved to vault. Add --visual to also read the video's frames (scene detection) | `.opencode/commands/youtube.md` |

### Meta - vault setup, health, structure

| Command | What it does | Read this file |
|---|---|---|
| `/create-command` | Create a new obsidian-second-brain command via interview - zero markdown editing required | `.opencode/commands/create-command.md` |
| `/obsidian-architect` | Scan a codebase and write a maintained set of architecture notes into the vault - overview, per-module notes, key decisions. Re-run to refresh without clobbering your edits | `.opencode/commands/obsidian-architect.md` |
| `/obsidian-export` | Export a clean structured snapshot of the vault that any agent or tool can consume - flat JSON, markdown index, or an OKF (Open Knowledge Format) bundle | `.opencode/commands/obsidian-export.md` |
| `/obsidian-health` | Run a vault health check - grouped by severity, detects contradictions, concept gaps, stale claims, and structural issues | `.opencode/commands/obsidian-health.md` |
| `/obsidian-init` | Scan your vault and generate a _CLAUDE.md operating manual, index.md catalog, and log.md pointer | `.opencode/commands/obsidian-init.md` |
| `/obsidian-reindex` | Refresh the vault's semantic search index and report coverage before and after | `.opencode/commands/obsidian-reindex.md` |
| `/obsidian-retrieval-eval` | Measure how well vault search finds the right note for a natural-language question - recall@k and MRR, with the concrete failures | `.opencode/commands/obsidian-retrieval-eval.md` |
| `/obsidian-visualize` | Generate a visual canvas map of your vault - see the shape of your second brain and how knowledge connects | `.opencode/commands/obsidian-visualize.md` |

## Trigger phrases

When the user says any of the phrases below (or a close paraphrase), follow the matching command file.

### English (`en`)


**Vault - daily writing, capture, find**

- `/obsidian-board-hygiene` - "clean up my board", "triage my board", "board hygiene", "archive stale tasks", "my board is a mess"
- `/obsidian-board` - "show board", "kanban", "what is on my board", "update board"
- `/obsidian-capture` - "capture this idea", "save this idea", "quick note", "drop a thought"
- `/obsidian-catchup` - "catch up", "catchup", "what did I dump from telegram", "process my captures", "go through my telegram dumps", "anything new from the phone", "process my catchup", "review what I captured", "what did I capture on the go"
- `/obsidian-daily` - "todays note", "create todays daily", "open daily", "today daily note"
- `/obsidian-find` - "find in vault", "search my notes", "where is", "what did I write about"
- `/obsidian-log` - "log this work", "log this session", "log this dev session", "obsidian log"
- `/obsidian-person` - "save this person", "add person", "new contact note", "create person note"
- `/obsidian-project` - "new project", "create project note", "project setup", "start a project"
- `/obsidian-projects` - "projects overview", "project status", "what am I working on", "show projects"
- `/obsidian-recap` - "recap today", "recap the week", "summarize the week", "month recap"
- `/obsidian-recurring` - "recurring task", "monthly obligation", "remind me every month", "recurring payment", "track a recurring"
- `/obsidian-save` - "save this", "save the conversation", "save to vault", "obsidian save"
- `/obsidian-task` - "add task", "new todo", "track this", "remind me"
- `/obsidian-world` - "load context", "what is going on", "where am I", "load my world"

**Thinking - synthesis, decisions, learning, reviews**

- `/idea-discovery` - "what should I work on next", "idea discovery", "surface next directions", "what's worth pursuing"
- `/obsidian-brainstorm` - "brainstorm this", "brainstorm with me", "think this through with me", "help me design this", "interview me about this idea"
- `/obsidian-challenge` - "challenge this", "grill me on this", "red team my idea", "stress test this"
- `/obsidian-connect` - "connect domains", "cross-pollinate", "bridge ideas", "find an unexpected link"
- `/obsidian-decide` - "extract decisions", "log decisions", "what did we decide", "log this decision", "ADR", "record decision", "decision record"
- `/obsidian-distill` - "distill this", "condense this note", "summarize with sources", "distill this source", "boil this down with provenance"
- `/obsidian-emerge` - "find patterns", "what is emerging", "surface themes", "unnamed patterns"
- `/obsidian-graduate` - "promote idea", "graduate this to project", "make a project from this", "elevate idea"
- `/obsidian-learn` - "review learnings", "what have I learned", "show lessons", "prune learnings"
- `/obsidian-panel` - "convene a panel", "advisor panel", "get multiple perspectives on", "panel review", "what would the experts say about"
- `/obsidian-reconcile` - "find contradictions", "reconcile vault", "fix conflicts", "vault contradictions"
- `/obsidian-review` - "weekly review", "monthly review", "review my week", "review my month"
- `/obsidian-synthesize` - "synthesize", "auto-synthesis", "make synthesis notes", "find unnamed patterns"
- `/vault-deep-synthesis` - "synthesize what I know about", "deep synthesis on", "cross-reference my notes on", "what does my vault say about"

**Research - bring external sources into the vault**

- `/notebooklm` - "notebooklm", "research grounded", "ground research in vault", "ask my notebook", "source-grounded research"
- `/obsidian-ingest` - "ingest this source", "add this article", "import this", "absorb this"
- `/podcast` - "summarize this podcast", "podcast episode summary", "extract podcast", "what's in this episode"
- `/research-deep` - "deep research", "thorough research", "vault-first research", "research gaps"
- `/research` - "research this", "look up", "find information about", "perplexity research"
- `/x-pulse` - "x pulse", "what is trending on twitter", "scan x for", "twitter pulse"
- `/x-read` - "read this x post", "deep read this tweet", "analyze this tweet", "read this thread"
- `/youtube` - "summarize youtube", "youtube transcript", "extract video", "youtube to vault", "watch this video", "what's on screen in this video"

**Meta - vault setup, health, structure**

- `/create-command` - "create command", "new command", "add a command", "scaffold a command"
- `/obsidian-architect` - "document this codebase", "architect this project", "map this code into my vault", "generate architecture notes", "refresh architecture docs"
- `/obsidian-export` - "export vault", "snapshot vault", "dump vault", "vault export"
- `/obsidian-health` - "vault health", "check vault", "audit vault", "vault diagnostics"
- `/obsidian-init` - "init vault", "bootstrap vault", "setup vault", "scan vault"
- `/obsidian-reindex` - "reindex vault", "rebuild semantic index", "refresh semantic search", "update vault embeddings"
- `/obsidian-retrieval-eval` - "evaluate retrieval", "how good is my vault search", "retrieval eval", "test vault search quality", "measure find quality"
- `/obsidian-visualize` - "visualize vault", "vault map", "canvas of vault", "show me the vault shape"

### Español (`es`)


**Vault - daily writing, capture, find**

- `/obsidian-board-hygiene` - "limpia mi tablero", "haz triaje de mi tablero", "ordena el tablero", "archiva las tareas viejas", "mi tablero es un desastre"
- `/obsidian-board` - "muestra el tablero", "kanban", "qué hay en mi tablero", "actualiza el tablero", "cómo va mi tablero"
- `/obsidian-capture` - "captura esta idea", "guarda esta idea", "nota rápida", "apunta esto"
- `/obsidian-catchup` - "ponme al día", "qué mandé por telegram", "procesa mis capturas", "revisa lo que capturé desde el móvil", "hay algo nuevo del móvil", "repasa mi cola de capturas"
- `/obsidian-daily` - "nota de hoy", "crea la diaria de hoy", "abre mi diaria", "abre la nota de hoy", "dame mi diaria"
- `/obsidian-find` - "busca en el vault", "busca en mis notas", "dónde está", "qué escribí sobre", "tengo algo sobre"
- `/obsidian-log` - "registra este trabajo", "registra esta sesión", "registra esta sesión de desarrollo", "obsidian log"
- `/obsidian-person` - "guarda a esta persona", "añade una persona", "nueva nota de contacto", "crea una nota de persona"
- `/obsidian-project` - "nuevo proyecto", "crea una nota de proyecto", "configura el proyecto", "arranca un proyecto", "crea un proyecto nuevo"
- `/obsidian-projects` - "resumen de proyectos", "estado de los proyectos", "en qué estoy trabajando", "muéstrame los proyectos"
- `/obsidian-recap` - "resumen de hoy", "resumen de la semana", "resume la semana", "resumen del mes"
- `/obsidian-recurring` - "tarea recurrente", "obligación mensual", "recuérdamelo cada mes", "pago recurrente", "haz seguimiento de algo recurrente"
- `/obsidian-save` - "guarda esto", "guarda la conversación", "guarda al vault", "guarda todo esto"
- `/obsidian-task` - "añade una tarea", "nuevo pendiente", "haz seguimiento de esto", "recuérdamelo"
- `/obsidian-world` - "carga el contexto", "qué está pasando", "dónde estoy", "ponme al tanto de todo", "dame el resumen de mi vida"

**Thinking - synthesis, decisions, learning, reviews**

- `/idea-discovery` - "descubre ideas para seguir", "qué rumbos tomar", "qué vale la pena perseguir", "en qué me enfoco ahora", "dame ideas para seguir"
- `/obsidian-brainstorm` - "haz una lluvia de ideas conmigo", "piensa esto conmigo", "ayúdame a diseñar esto", "entrevístame sobre esta idea", "dale vueltas a esto conmigo"
- `/obsidian-challenge` - "cuestiona esta idea", "ponme a prueba con esto", "haz de abogado del diablo con mi idea", "pon esto a prueba", "dime por qué esto no funcionaría"
- `/obsidian-connect` - "conecta estos dos temas", "cruza ideas de distintos mundos", "busca una conexión inesperada", "conecta esto con algo que no tenga que ver"
- `/obsidian-decide` - "saca las decisiones de esta conversación", "registra las decisiones", "¿qué decidimos?", "anota esta decisión", "ADR", "acta de decisión formal"
- `/obsidian-distill` - "destila esto", "condensa esta nota", "resúmelo con fuentes", "destila esta fuente", "resume esto sin perder el contexto", "sácame lo importante de esto con las fuentes"
- `/obsidian-emerge` - "busca patrones", "qué está emergiendo", "patrones que no he nombrado", "qué patrones ves aquí", "qué se repite en mis notas"
- `/obsidian-graduate` - "promociona esta idea", "convierte esto en proyecto", "haz un proyecto de esto", "eleva esta idea"
- `/obsidian-learn` - "revisa los aprendizajes", "qué he aprendido", "muéstrame las lecciones", "poda los aprendizajes"
- `/obsidian-panel` - "convoca un panel", "panel de asesores", "dame varias perspectivas sobre esto", "revisión en panel", "qué dirían los expertos sobre esto"
- `/obsidian-reconcile` - "busca contradicciones", "concilia el vault", "resuelve los conflictos", "contradicciones en el vault"
- `/obsidian-review` - "revisión semanal", "revisión mensual", "revisa mi semana", "revisa mi mes"
- `/obsidian-synthesize` - "sintetiza", "síntesis automática", "crea notas de síntesis", "busca patrones nuevos", "saca conclusiones de mis notas"
- `/vault-deep-synthesis` - "sintetiza lo que sé sobre", "síntesis profunda sobre", "cruza mis notas sobre", "qué dice mi vault sobre"

**Research - bring external sources into the vault**

- `/notebooklm` - "notebooklm", "investigación fundamentada en mis notas", "basa esto en mi vault", "pregúntale a mis notas", "investigación con fuentes propias", "investiga en mis notas"
- `/obsidian-ingest` - "haz una ingesta de esta fuente", "añade este artículo", "importa esto", "absorbe esto", "mete esto al vault", "procesa esta fuente"
- `/podcast` - "resume este pódcast", "resumen del episodio", "extrae este pódcast", "qué dice este episodio"
- `/research-deep` - "investigación profunda", "investiga a fondo", "investigación basada en mi vault", "rellena los huecos de información"
- `/research` - "investiga esto", "búscalo", "busca información sobre", "investigación con perplexity"
- `/x-pulse` - "x pulse", "qué es tendencia en twitter", "escanea x en busca de", "pulso de twitter"
- `/x-read` - "léeme este post de x", "profundiza en este tweet", "analiza este tweet", "léeme este hilo"
- `/youtube` - "resume este vídeo de youtube", "transcripción de youtube", "extrae este vídeo", "youtube al vault", "mira este vídeo", "qué se ve en este vídeo"

**Meta - vault setup, health, structure**

- `/create-command` - "crea un comando", "nuevo comando", "añade un comando", "hazme un comando nuevo", "arma un comando"
- `/obsidian-architect` - "documenta este código", "analiza la arquitectura de este proyecto", "lleva este código a mi vault", "genera notas de arquitectura", "actualiza la documentación de arquitectura", "pon la arquitectura en el vault"
- `/obsidian-export` - "exporta el vault", "haz una foto del vault", "vuelca el vault", "exportación del vault"
- `/obsidian-health` - "salud del vault", "revisa el vault", "audita el vault", "diagnóstico del vault"
- `/obsidian-init` - "inicializa el vault", "arranca el vault", "configura el vault", "escanea el vault", "inicia el vault", "deja listo el vault"
- `/obsidian-reindex` - "reindexa el vault", "reconstruye el índice semántico", "actualiza la búsqueda semántica", "actualiza los embeddings del vault"
- `/obsidian-retrieval-eval` - "evalúa la búsqueda", "qué tal funciona la búsqueda de mi vault", "evaluación de recuperación", "prueba la calidad de la búsqueda", "comprueba si encuentra bien mis notas"
- `/obsidian-visualize` - "visualiza el vault", "mapa del vault", "canvas del vault", "muéstrame la forma de mi vault"

### Português (`pt`)


**Vault - daily writing, capture, find**

- `/obsidian-board-hygiene` - "limpe meu board", "faça a triagem do meu board", "higiene do board", "arquive tarefas paradas", "meu board está uma bagunça"
- `/obsidian-board` - "mostre o board", "kanban", "o que está no meu board", "atualize o board"
- `/obsidian-capture` - "capture esta ideia", "salve esta ideia", "anotação rápida", "registre um pensamento"
- `/obsidian-catchup` - "coloque em dia", "catchup", "o que eu despejei do telegram", "processe minhas capturas", "passe pelas minhas capturas do telegram", "tem algo novo do telefone", "processe meu catchup", "revise o que capturei", "o que capturei na correria"
- `/obsidian-daily` - "nota de hoje", "crie a nota diária de hoje", "abra a diária", "nota diária de hoje"
- `/obsidian-find` - "encontre no vault", "pesquise minhas notas", "onde está", "o que eu escrevi sobre"
- `/obsidian-log` - "registre este trabalho", "registre esta sessão", "registre esta sessão de desenvolvimento", "obsidian log"
- `/obsidian-person` - "salve esta pessoa", "adicione uma pessoa", "nova nota de contato", "crie uma nota de pessoa"
- `/obsidian-project` - "novo projeto", "crie uma nota de projeto", "configuração de projeto", "inicie um projeto"
- `/obsidian-projects` - "visão geral de projetos", "status dos projetos", "em que estou trabalhando", "mostre os projetos"
- `/obsidian-recap` - "recapitule hoje", "recapitule a semana", "resuma a semana", "recap do mês"
- `/obsidian-recurring` - "tarefa recorrente", "obrigação mensal", "me lembre todo mês", "pagamento recorrente", "acompanhe algo recorrente"
- `/obsidian-save` - "salve isto", "salve a conversa", "salve no vault", "obsidian save"
- `/obsidian-task` - "adicione uma tarefa", "novo a fazer", "acompanhe isto", "me lembre disto"
- `/obsidian-world` - "carregue contexto", "o que está acontecendo", "onde estou", "carregue meu mundo"

**Thinking - synthesis, decisions, learning, reviews**

- `/idea-discovery` - "em que devo trabalhar agora", "descoberta de ideias", "mostre próximos caminhos", "o que vale a pena perseguir"
- `/obsidian-brainstorm` - "faça um brainstorm comigo", "pense nisso comigo", "me ajude a desenhar isto", "me entreviste sobre esta ideia"
- `/obsidian-challenge` - "desafie isto", "questione minha ideia", "faça um red team da minha ideia", "teste esta ideia"
- `/obsidian-connect` - "conecte domínios", "cruze ideias", "crie pontes entre ideias", "encontre uma ligação inesperada"
- `/obsidian-decide` - "extraia decisões", "registre decisões", "o que decidimos", "registre esta decisão", "adr", "registrar decisão", "registro de decisão"
- `/obsidian-distill` - "destile isto", "condense esta nota", "resuma com fontes", "destile esta fonte", "reduza isto com proveniência"
- `/obsidian-emerge` - "encontre padrões", "o que está emergindo", "mostre temas", "padrões sem nome"
- `/obsidian-graduate` - "promova esta ideia", "transforme isto em projeto", "crie um projeto a partir disto", "eleve esta ideia"
- `/obsidian-learn` - "revise aprendizados", "o que eu aprendi", "mostre lições", "reduza aprendizados"
- `/obsidian-panel` - "convoque um painel", "painel de especialistas", "obtenha múltiplas perspectivas sobre", "revisão em painel", "o que os especialistas diriam sobre"
- `/obsidian-reconcile` - "encontre contradições", "reconcilie o vault", "corrija conflitos", "contradições do vault"
- `/obsidian-review` - "revisão semanal", "revisão mensal", "revise minha semana", "revise meu mês"
- `/obsidian-synthesize` - "sintetize", "auto-síntese", "crie notas de síntese", "encontre padrões sem nome"
- `/vault-deep-synthesis` - "sintetize o que eu sei sobre", "síntese profunda sobre", "cruze minhas notas sobre", "o que meu vault diz sobre"

**Research - bring external sources into the vault**

- `/notebooklm` - "notebooklm", "pesquisa ancorada", "ancore a pesquisa no vault", "pergunte ao meu notebook", "pesquisa ancorada em fontes"
- `/obsidian-ingest` - "ingira esta fonte", "adicione este artigo", "importe isto", "absorva isto"
- `/podcast` - "resuma este podcast", "resumo de episódio de podcast", "extraia este podcast", "o que há neste episódio"
- `/research-deep` - "pesquisa profunda", "pesquisa completa", "pesquisa com base no vault", "lacunas de pesquisa"
- `/research` - "pesquise isto", "procure", "encontre informações sobre", "pesquisa perplexity"
- `/x-pulse` - "x pulse", "o que está em alta no x", "escaneie o x em busca de", "pulso do twitter"
- `/x-read` - "leia este post do x", "leitura profunda deste tweet", "analise este tweet", "leia esta thread"
- `/youtube` - "resuma este vídeo do youtube", "transcrição do youtube", "extraia este vídeo", "youtube para o vault", "assista a este vídeo", "o que aparece neste vídeo"

**Meta - vault setup, health, structure**

- `/create-command` - "crie um comando", "novo comando", "adicione um comando", "gere um comando"
- `/obsidian-architect` - "documente este código", "arquitete este projeto", "mapeie este código no vault", "gere notas de arquitetura", "atualize a documentação de arquitetura"
- `/obsidian-export` - "exporte o vault", "gere um snapshot do vault", "despeje o vault", "exportação do vault"
- `/obsidian-health` - "saúde do vault", "verifique o vault", "audite o vault", "diagnóstico do vault"
- `/obsidian-init` - "inicialize o vault", "bootstrap do vault", "configure o vault", "escaneie o vault"
- `/obsidian-reindex` - "reindexe o vault", "reconstrua o índice semântico", "atualize a busca semântica", "atualize os embeddings do vault"
- `/obsidian-retrieval-eval` - "avalie a recuperação", "quão boa é a busca do meu vault", "avaliação de recuperação", "teste a qualidade da busca do vault", "meça a qualidade da busca"
- `/obsidian-visualize` - "visualize o vault", "mapa do vault", "canvas do vault", "mostre a forma do vault"

### 简体中文 (`zh`)


**Vault - daily writing, capture, find**

- `/obsidian-board-hygiene` - "整理我的看板", "清理过期任务", "帮我归档看板上的旧任务", "我的看板太乱了", "批量梳理看板"
- `/obsidian-board` - "打开我的看板", "看看看板上有什么", "更新看板", "查看任务看板"
- `/obsidian-capture` - "记下这个想法", "帮我快速记一笔", "先把这个灵感存下来", "随手记一下"
- `/obsidian-catchup` - "看看我从手机记了什么", "处理 Telegram 收集箱", "整理我路上记的东西", "整理待处理的随手记录", "把最近随手记的内容过一遍"
- `/obsidian-daily` - "打开今天的日记", "创建今天的每日笔记", "更新今天的日记", "看看今天要做什么"
- `/obsidian-find` - "在知识库里找一下", "搜索我的笔记", "我之前在哪篇笔记里写过", "我写过关于这个吗"
- `/obsidian-log` - "记录这次工作", "把这次开发过程写进知识库", "记录当前工作会话", "保存这次开发日志"
- `/obsidian-person` - "保存这个人的信息", "新建联系人笔记", "为这个人建档", "更新这个人的资料"
- `/obsidian-projects` - "看看所有项目的状态", "我现在在做哪些项目", "项目进展怎么样", "显示项目总览"
- `/obsidian-project` - "创建一个新项目", "新建项目笔记", "开始这个项目", "为项目建立基本结构"
- `/obsidian-recap` - "总结今天发生的事", "汇总本周记录", "总结这一周发生了什么", "生成本月摘要"
- `/obsidian-recurring` - "添加一个周期任务", "每个月提醒我", "记录这项定期付款", "跟踪一个重复事项", "记录一件要定期处理的事"
- `/obsidian-save` - "保存这段对话", "把值得保留的内容存进知识库", "把刚才聊的整理进笔记", "保存到我的知识库"
- `/obsidian-task` - "添加一个任务", "记个待办", "跟踪这件事", "提醒我处理这个", "把这件事放到看板"
- `/obsidian-world` - "加载我的完整背景", "告诉我现在的整体状况", "读取我的身份和当前重点", "先了解一下我的情况"

**Thinking - synthesis, decisions, learning, reviews**

- `/idea-discovery` - "我接下来该做什么", "帮我找下一个方向", "有哪些想法值得继续", "从笔记里发现新方向"
- `/obsidian-brainstorm` - "陪我头脑风暴", "和我一起把这个想清楚", "通过提问帮我完善这个想法", "帮我设计这个方案"
- `/obsidian-challenge` - "挑战一下这个想法", "帮我找这个方案的问题", "站在反方审视它", "给这个想法做压力测试", "别客气地质疑我"
- `/obsidian-connect` - "把这两个领域联系起来", "帮我找跨领域连接", "看看这些想法有什么意外联系", "用我的笔记做跨界联想"
- `/obsidian-decide` - "记录这个决定", "我们刚才决定了什么", "把这些决策整理出来", "生成决策记录", "写一份 ADR"
- `/obsidian-distill` - "提炼这篇笔记", "把这个内容压缩成关键结论", "总结这份材料并标明出处", "精简内容但保留来源", "从这个来源提炼要点"
- `/obsidian-emerge` - "从最近的笔记里找规律", "最近有什么趋势正在浮现", "找出我还没说清的模式", "看看反复出现的主题"
- `/obsidian-graduate` - "把这个想法变成项目", "将这条灵感升级为项目", "为这个想法建立完整项目", "把它拆成项目和任务"
- `/obsidian-learn` - "回顾我学到的东西", "我最近学到了什么", "整理知识库里的经验", "清理过时的学习记录"
- `/obsidian-panel` - "找几个不同视角来评审", "让多个专家分析这个决定", "从几个角度评价这个方案", "开个顾问评审会", "不同专家会怎么看"
- `/obsidian-reconcile` - "找出知识库里的矛盾", "解决笔记之间的冲突", "核对相互矛盾的说法", "让知识库里的事实保持一致"
- `/obsidian-review` - "做每周复盘", "做月度复盘", "回顾我这一周的表现", "生成本月复盘"
- `/obsidian-synthesize` - "自动综合我的知识库", "生成综合笔记", "把未命名的模式写成总结", "扫描知识库并形成综合结论"
- `/vault-deep-synthesis` - "综合知识库里关于这个主题的一切", "深度梳理我对这个主题的笔记", "交叉核对这个主题的所有记录", "我的知识库对这个主题怎么说"

**Research - bring external sources into the vault**

- `/notebooklm` - "用我的资料做研究", "基于知识库回答", "问问我的笔记", "做有来源依据的研究", "用 NotebookLM 研究"
- `/obsidian-ingest` - "把这篇文章纳入知识库", "导入这份资料", "用这个来源更新我的笔记", "消化这份材料"
- `/podcast` - "总结这期播客", "提取播客文字稿", "这期节目讲了什么", "把这期播客整理进知识库"
- `/research-deep` - "做一次深度研究", "基于我的知识库深入研究", "补齐这个主题的研究空白", "全面调查这个问题"
- `/research` - "研究一下这个问题", "帮我查资料", "搜索关于这个主题的信息", "做一份带引用的网络研究"
- `/x-pulse` - "看看 X 上这个话题的趋势", "推特上这个话题有什么热点", "扫描 X 上的热门讨论", "分析这个主题的社交媒体风向"
- `/x-read` - "读一下这条 X 帖子", "深入分析这条推文", "这条推文讲了什么", "梳理一下这串推文"
- `/youtube` - "总结这个 YouTube 视频", "提取视频字幕", "把这个视频整理进知识库", "这个视频讲了什么", "看看视频画面里有什么"

**Meta - vault setup, health, structure**

- `/create-command` - "创建一个新命令", "新增 Obsidian 命令", "帮我写个命令", "生成命令模板"
- `/obsidian-architect` - "给这个代码库写架构文档", "分析这个项目的架构", "把代码结构整理进知识库", "生成架构笔记", "更新架构文档"
- `/obsidian-export` - "导出知识库", "给知识库做个快照", "把笔记导成 JSON", "生成 OKF 知识包", "导出一份可移植的数据"
- `/obsidian-health` - "检查知识库健康状况", "给我的知识库做体检", "审计我的笔记库", "诊断知识库问题"
- `/obsidian-init` - "初始化知识库", "为这个知识库生成初始配置", "扫描并配置我的知识库", "生成知识库操作手册"
- `/obsidian-reindex` - "重建知识库索引", "刷新语义搜索", "更新笔记向量", "重新索引我的知识库"
- `/obsidian-retrieval-eval` - "测试知识库搜索效果", "评估检索质量", "看看我的笔记好不好找", "测量语义搜索召回率", "检查搜索能不能找到正确笔记"
- `/obsidian-visualize` - "把知识库可视化", "生成知识图谱画布", "画出我的笔记关系", "看看知识库的整体结构"

---

*Generated by adapters/opencode/adapter.sh - do not edit manually.*
