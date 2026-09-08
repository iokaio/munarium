# Local Ollama evaluation

This isolated Docker Compose project runs Ollama and PostgreSQL on Docker Desktop
for Windows in Linux container mode. The optional `server` profile builds Munarium
from this checkout. Server 1.1 supports the local provider; the initial environment
checks below call Ollama directly.

Run these commands in PowerShell from `server/deploy/ollama`:

```powershell
docker compose config --quiet
docker compose up -d --wait ollama postgres
docker compose exec -T ollama ollama pull qwen3:1.7b
docker compose exec -T ollama ollama pull all-minilm:22m
docker compose exec -T ollama ollama list
Invoke-RestMethod http://127.0.0.1:11434/api/version
./Test-Ollama.ps1
```

Ollama is pinned to version 0.33.3 and its multi-platform image digest. The initial
model tags are `qwen3:1.7b` (approximately 1.4 GB) and `all-minilm:22m` (approximately
46 MB). Model tags can move: the smoke script compares their full digests to
[models.json](models.json) and fails if either changed. A changed digest requires
review and a new evaluation before updating that file. Model files stay in the
`munarium-ollama_models` named volume.

`Test-Ollama.ps1` checks four fixed short-answer prompts (including cold and warm
requests) and two 384-dimensional embeddings. It saves full model digests,
answers, token counts, and timings to the ignored
`server/scratch/ollama/environment.json`. These checks establish this local
environment's behavior; they do not establish general reasoning quality or
Munarium integration. Use `-Endpoint` if you remap the host port.

Ollama has a four-CPU, 6 GiB memory limit, a 4,096-token default generation
context, one concurrent request, and one loaded model. It uses CPU inference
without a GPU reservation. Cloud features are disabled; image and model downloads
still need Internet access. Model weights are not bundled into Munarium's image.
The Ollama health check confirms API availability, not that models are installed
or that inference works.

For a small completion, use:

```powershell
$chat = @{
    model = 'qwen3:1.7b'
    stream = $false
    think = $false
    messages = @(@{
        role = 'user'
        content = 'The project code is MAPLE. What is the project code? Reply with only the code.'
    })
    options = @{ temperature = 0; num_predict = 64 }
} | ConvertTo-Json -Depth 6
$answer = Invoke-RestMethod http://127.0.0.1:11434/api/chat -Method Post -ContentType 'application/json' -Body $chat -TimeoutSec 180
$answer.message.content
```

Qwen thinking is disabled explicitly so the output budget serves the answer.
Cold model loading can take longer than subsequent requests. The adapter's later
integration tests must also cover timeout and truncation behavior.

Generate two embeddings with:

```powershell
$embed = @{
    model = 'all-minilm:22m'
    input = @('The project code is MAPLE.', 'The meeting starts at noon.')
    truncate = $false
} | ConvertTo-Json
$vectors = Invoke-RestMethod http://127.0.0.1:11434/api/embed -Method Post -ContentType 'application/json' -Body $embed -TimeoutSec 180
$vectors.embeddings.Count
$vectors.embeddings[0].Count
```

Keep embedding inputs below this model's 512-token context. `truncate: false`
makes oversized input an error instead of silently losing document text.

The project network uses `http://ollama:11434`; Windows uses
`http://127.0.0.1:11434`. `localhost` inside a container refers to that container.
PostgreSQL is reachable only on the project network. The database password and
Munarium token in Compose are public local-evaluation credentials.

Build the optional Server and follow the [provider guide](../../docs/guides/ollama.md)
to register its models:

```powershell
docker compose --profile server up -d --build --wait
Invoke-RestMethod http://127.0.0.1:28080/readyz
```

For the live integration suite, install the repository's Python client test
dependencies in a virtual environment (`python -m pip install -e clients/python`
from the repository root), then run from this directory:

```powershell
python test_integration.py
docker compose --profile server up -d --no-deps --force-recreate server
# Wait for http://127.0.0.1:28080/readyz to return 200 before the next command.
python test_integration.py --verify-persisted ../../scratch/ollama/integration.json --output ../../scratch/ollama/persisted-integration.json
```

The suite covers REST/gRPC provider calls and cache parity, isolation from the
second test tenant, budgets and error paths, plus a unique two-document collection
and grounded answer. It approves only that synthetic collection's index cutover.
Index construction retains the existing local embedder; the Ollama embedding API
is tested separately. All evidence stays under the ignored `server/scratch/ollama`.

Server REST is on loopback port 28080 and gRPC on 25051. Its requests require
`Authorization: Bearer ollama-evaluation-token` and `X-Munarium-Uid: evaluator`.
The second token, `ollama-other-token`, selects a separate tenant for isolation tests.
Change `OLLAMA_PORT`, `MUNARIUM_OLLAMA_HTTP_PORT`, or `MUNARIUM_OLLAMA_GRPC_PORT` in
your shell if a host port is already occupied; adjust host-side requests too.
The default Compose project name keeps these resources separate from other local
Munarium deployments. Use a different project name with `-p` for another copy.

Inspect or stop the project without deleting its data:

```powershell
docker compose ps
docker compose logs --tail 50 ollama
docker compose --profile server down
```

Bringing the project up again reuses the models and PostgreSQL data. Adding `-v`
to `down` deletes both named volumes and their contents.

Upstream references: [Ollama Docker](https://docs.ollama.com/docker),
[Qwen3 model](https://ollama.com/library/qwen3:1.7b),
[embedding model](https://ollama.com/library/all-minilm:22m),
[chat API](https://docs.ollama.com/api/chat), and
[embedding API](https://docs.ollama.com/api/embed).
