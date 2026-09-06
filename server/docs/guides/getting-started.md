# Getting started with Munarium Server

This guide takes you from the published Docker image to a small, persistent
corpus application. You will write a fact through the API, define a shape and
runbook, upload two documents, build and approve an index, and retrieve its
evidence. The main walkthrough makes no model calls and needs no provider keys.

Use this guide after the [repository overview](../../../README.md). For build
internals, deployment variations and larger application patterns, continue with
the [developer guide](dev-guide.md).

## 1. Understand the pieces you are about to connect

| Piece | What it does in this walkthrough |
|---|---|
| Server | Serves authenticated REST and gRPC operations and coordinates the workflow. |
| PostgreSQL | Persists the ledger, application configuration, indexes and document bytes. |
| Shape | Describes how this document family is chunked and indexed. Shapes can also declare fact constraints and evidence semantics. |
| Runbook | Binds documents into collections and defines the build, verification, cutover and retrieval policy. |
| Source | A document identified by its logical filename, with a hash recording its content. |
| Collection and index | Group the permitted documents and make a built version searchable. |
| Session | Pins a runbook version and carries the caller's access context across turns. |

The fact ledger and corpus retrieval are related capabilities, but uploading
a document does not automatically assert every sentence as an accepted ledger
fact. You will exercise both paths explicitly. Retrieval returns evidence;
optional completion asks a configured model to turn that evidence into an answer.

## 2. Start a persistent local Server

You need PowerShell 7.3 or later and Docker Desktop running Linux containers.
The public `iokaio/munarium:1.0.0` image supports AMD64 and ARM64 and includes
the Server and `/mmctl` CLI. Matrix and your application UI are separate
deployments. No source checkout or Rust toolchain is required.

Work in a new, empty directory, and run the commands in order in the same
PowerShell session. Create private database and signing credentials:

```powershell
$ErrorActionPreference = 'Stop'
if (Test-Path .env) { throw 'Use an empty directory; keep existing deployment credentials' }
$dbPassword = [Convert]::ToHexString([Security.Cryptography.RandomNumberGenerator]::GetBytes(32))
$tokenSecret = [Convert]::ToHexString([Security.Cryptography.RandomNumberGenerator]::GetBytes(32))
@"
POSTGRES_PASSWORD=$dbPassword
MUNARIUM_TOKEN_SECRET=$tokenSecret
"@ | Set-Content .env -Encoding utf8
$dbPassword = $null
$tokenSecret = $null
```

Keep `.env` private and outside Git. Avoid setting the same variables in your
shell: shell values can override the file during Compose interpolation.
Save the following as `compose.yaml`:

```yaml
name: munarium-start
services:
  postgres:
    image: pgvector/pgvector:pg16
    environment:
      POSTGRES_USER: munarium
      POSTGRES_PASSWORD: ${POSTGRES_PASSWORD:?Set POSTGRES_PASSWORD in .env}
      POSTGRES_DB: munarium
    volumes:
      - pgdata:/var/lib/postgresql/data
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U munarium -d munarium"]
      interval: 3s
      timeout: 3s
      retries: 20
  server:
    image: iokaio/munarium:1.0.0
    depends_on:
      postgres:
        condition: service_healthy
    environment:
      MUNARIUM_STORE: postgres
      MUNARIUM_DATABASE_URL: "postgres://munarium:${POSTGRES_PASSWORD:?Set POSTGRES_PASSWORD in .env}@postgres:5432/munarium"
      MUNARIUM_SOURCE_STORE: pg
      MUNARIUM_RETRIEVAL_MODE: postgres
      MUNARIUM_AUTH_MODE: static
      MUNARIUM_STATIC_TOKENS: devtoken:dev-tenant:rw
      MUNARIUM_TOKEN_SECRET: ${MUNARIUM_TOKEN_SECRET:?Set MUNARIUM_TOKEN_SECRET in .env}
    ports:
      - "127.0.0.1:18080:8080"
      - "127.0.0.1:15051:50051"
volumes:
  pgdata:
```

The example's `devtoken` is a public, local-only credential mapping to tenant
`dev-tenant` with read/write access. The ports are bound to your machine's
loopback interface. Replace this credential and configure suitable ingress
and access controls before offering the service remotely.

Set both storage selectors explicitly. `MUNARIUM_STORE=postgres` selects the
persistent ledger; `MUNARIUM_SOURCE_STORE=pg` stores raw documents in the same
database. Omitting the latter with a PostgreSQL ledger selects the Azure Blob
default and requires Azure configuration. This setup needs no Server data
volume because persistence resides in PostgreSQL's `pgdata` volume.

```powershell
docker compose config --quiet
if ($LASTEXITCODE -ne 0) { throw 'Invalid Compose configuration' }
docker compose pull
if ($LASTEXITCODE -ne 0) { throw 'Image pull failed' }
docker compose up -d
if ($LASTEXITCODE -ne 0) { throw 'Compose startup failed' }
$base = 'http://127.0.0.1:18080'
function Wait-MunariumReady {
    $deadline = [DateTime]::UtcNow.AddMinutes(2)
    do {
        try {
            if ((Invoke-WebRequest "$base/readyz" -TimeoutSec 5).StatusCode -eq 200) { return }
        } catch { }
        Start-Sleep -Seconds 2
    } while ([DateTime]::UtcNow -lt $deadline)
    throw 'Server did not become ready; inspect docker compose logs server postgres'
}
Wait-MunariumReady
Invoke-RestMethod "$base/version"
```

The Server applies its embedded database migrations, including enabling
pgvector, during startup. Open `http://localhost:18080/admin` for the dashboard
and `http://localhost:18080/docs` for the API documentation. Direct gRPC is
available on port 15051; this walkthrough uses REST. If a host port is busy,
change the left-hand port in Compose and update `$base` accordingly.

`1.0.0` is immutable; `1.0` and `latest` can advance. For deployments you need
to reproduce exactly, pin the verified image digest from the
[release notes](https://github.com/iokaio/munarium/releases/tag/v1.0.0).
The [deployment walkthrough](dev-guide.md#deploy-the-published-docker-hub-image)
covers signatures, external PostgreSQL, backups and image upgrades in detail.

## 3. Make an authenticated ledger write

The bearer token selects the tenant and role. `X-Munarium-Uid` identifies the
caller within that tenant. Core writes also carry an idempotency key so a
retry can refer to the same operation. Use a new key for a different write.

```powershell
$headers = @{ Authorization='Bearer devtoken'; 'X-Munarium-Uid'='user-1' }
$writeHeaders = $headers.Clone()
$writeHeaders['Idempotency-Key'] = [guid]::NewGuid().ToString()
$version = Invoke-RestMethod "$base/v1/versions" -Method Post -Headers $writeHeaders -ContentType application/json -Body '{}'
$versionId = $version.version_id
$writeHeaders['Idempotency-Key'] = [guid]::NewGuid().ToString()
$claim = @{claim_type='fact';subject='starter';key='purpose';value='corpus evaluation'} | ConvertTo-Json
Invoke-RestMethod "$base/v1/versions/$versionId/claims" -Method Post -Headers $writeHeaders -ContentType application/json -Body $claim
Invoke-RestMethod "$base/v1/versions/$versionId/facts" -Headers $headers
```

Read the claim status as well as the HTTP result. Governance can record a
claim as disputed; a successful request does not mean every submitted claim
became accepted truth. The fact above is an uncomplicated first write into a
fresh lineage. Keep `$versionId` for the persistence check at the end.

## 4. Define a shape and a runbook

Save this as `starter-shape.yaml`. It uses the supported paragraph chunker and
a modest chunk size for the two short documents below. These are teaching
values for a tiny corpus, not a sizing recommendation for your real collection.

```yaml
apiVersion: munarium.ioka.io/v1
kind: Shape
metadata: { name: starter-documents, version: 1 }
spec:
  chunking: { strategy: para@1, max_chars: 800 }
  indexing: { rrf_k: 60, candidate_n: 50 }
```

Save the following as `starter-runbook.yaml`. It binds filenames beginning
with `starter/` to one collection and requires approval before making the
built index active. The completion policy is ready for a later model-backed
step, but the retrieval test explicitly leaves completion disabled.

```yaml
apiVersion: munarium.ioka.io/v1
kind: Runbook
metadata: { name: starter, version: 1 }
spec:
  sources: { prefix: "starter/" }
  collections:
    - name: starter-documents
      shape: starter-documents@1
      accessLevel: 0
      sources: { filenamePrefix: "starter/", mediaTypes: [text/plain] }
  retrieval: { topK: 4, rrfK: 60, candidateN: 50 }
  completion:
    contextCharBudget: 4000
    maxTokens: 2048
    promptTemplate: |
      Answer only from the supplied evidence and cite the source you use.
      If the evidence cannot answer the question, say what is missing.
      Context: {context}
      Question: {query}
  steps:
    - resolveSources: {}
    - buildIndex: {}
    - verify: {}
    - cutover: { approval: required }
    - retireOld: { keep_versions: 2 }
```

Apply the shape first because the runbook refers to its exact version. Validate
the runbook, inspect its findings, and apply it when there are no errors:

```powershell
$shapeYaml = Get-Content ./starter-shape.yaml -Raw
Invoke-RestMethod "$base/v1/shapes" -Method Post -Headers $headers -ContentType text/yaml -Body $shapeYaml
$runbookYaml = Get-Content ./starter-runbook.yaml -Raw
$validation = Invoke-RestMethod "$base/v1/runbooks/validate" -Method Post -Headers $headers -ContentType text/yaml -Body $runbookYaml
$validation.findings | Format-Table severity, code, message
if (-not $validation.valid) { throw 'Fix runbook validation errors before applying' }
Invoke-RestMethod "$base/v1/runbooks" -Method Post -Headers $headers -ContentType text/yaml -Body $runbookYaml
```

Applying a runbook registers its configuration; running it executes the index
workflow. Neither operation invents or downloads its corpus. Keep shape and
runbook files in your application repository and version changes deliberately.
Use a new version rather than replacing a published document's contents under
the same version identifier.

## 5. Ingest two documents

The sample text is fictional. Upload it through the Server API, retaining the
logical filenames that match the runbook's prefix:

```powershell
$documents = @(
    @{filename='starter/billing.txt'; text='ExampleWorks invoice questions are handled by the billing team. Include the invoice number in your request.'},
    @{filename='starter/accounts.txt'; text='ExampleWorks account access questions are handled by the account support team.'}
)
foreach ($document in $documents) {
    $payload = @{
        filename = $document.filename
        media_type = 'text/plain'
        content_base64 = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($document.text))
    } | ConvertTo-Json
    $ingested = Invoke-RestMethod "$base/v1/ingest" -Method Post -Headers $headers -ContentType application/json -Body $payload
    if ($ingested.error -or 'starter-documents' -notin $ingested.bound_to) { throw 'Document was not bound to the expected collection' }
    $ingested | Select-Object filename, source_id, existed, bound_to
}
```

The response names the source and its bound collections. Re-uploading the same
filename and bytes is recognized as an existing document; changing its bytes
requires an index rebuild before expecting updated retrieval. Filenames matter:
an upload outside `starter/` will not match this collection automatically.
For your own PDFs, office documents and bulk loads, follow
[Loading corpora](loading-corpora.md) and inspect extraction results.

## 6. Build, inspect and approve the index

Start a run and inspect the step results. In this small example, execution
reaches the cutover approval gate before returning:

```powershell
$run = Invoke-RestMethod "$base/v1/runbooks/starter/runs" -Method Post -Headers $headers
$runId = $run.run_id
$status = Invoke-RestMethod "$base/v1/runs/$runId" -Headers $headers
$status.steps | Format-Table ordinal, name, state, detail -Wrap
if ($status.state -ne 'awaiting_approval') { throw 'Inspect the run before proceeding; expected the cutover approval gate' }
$approvalStep = @($status.steps | Where-Object { $_.name -eq 'cutover:starter-documents' -and $_.state -eq 'awaiting_approval' })
if ($approvalStep.Count -ne 1) { throw 'Expected one pending cutover step' }
```

Confirm source resolution, index construction and verification succeeded.
Approval makes the verified index active; it is a deployment decision, not a
button to bypass a failed build. For these two inspected sample documents,
continue with:

```powershell
Invoke-RestMethod "$base/v1/runs/$runId/steps/$($approvalStep[0].ordinal)/approve" -Method Post -Headers $headers
$status = Invoke-RestMethod "$base/v1/runs/$runId" -Headers $headers
$status | Select-Object run_id, state
if ($status.state -ne 'done') { throw 'Inspect the run: it did not complete' }
```

The step ordinal comes from the run status. Do not hard-code it in a general
application: adding collections or changing the execution plan can change the
step layout. You can also use the bundled `/mmctl` for apply, run, status and
approval operations; see [the CLI guide](../ops/mmctl.md).

## 7. Retrieve evidence in a session

Create a session pinned to the runbook version and send a turn with completion
disabled. This sample uses the Server's local retrieval/indexing path and no
provider-backed query expansion, so it requires no external model service.

```powershell
$session = Invoke-RestMethod "$base/v1/runbooks/starter@1/sessions" -Method Post -Headers $headers
$session | Select-Object session_id, runbook_ref, permitted_collections
$question = @{query='Who handles invoice questions?';complete=$false} | ConvertTo-Json
$answer = Invoke-RestMethod "$base/v1/sessions/$($session.session_id)/turns" -Method Post -Headers $headers -ContentType application/json -Body $question
$answer.hits | Select-Object source_path, text, score
if (-not @($answer.hits | Where-Object { $_.source_path -eq 'starter/billing.txt' -and $_.text -match 'billing team' }).Count) {
    throw 'Expected billing evidence was not retrieved'
}
```

You should find the billing document and its supporting text. Inspect
`collections_searched`, `skipped`, `hits` and `envelopes` in the full response:
they distinguish searched collections, unavailable indexes, document evidence
and retrieval provenance. These hits are not a generated answer or proof that
every relevant document has been found.

Now recreate only the Server and verify the earlier fact is still present:

```powershell
docker compose up -d --no-deps --force-recreate server
if ($LASTEXITCODE -ne 0) { throw 'Server recreation failed' }
Wait-MunariumReady
$facts = Invoke-RestMethod "$base/v1/versions/$versionId/facts" -Headers $headers
if (-not @($facts.facts | Where-Object { $_.subject -eq 'starter' -and $_.key -eq 'purpose' -and $_.value -eq 'corpus evaluation' }).Count) {
    throw 'The fact did not survive Server recreation'
}
$answer = Invoke-RestMethod "$base/v1/sessions/$($session.session_id)/turns" -Method Post -Headers $headers -ContentType application/json -Body $question
if (-not @($answer.hits | Where-Object { $_.source_path -eq 'starter/billing.txt' }).Count) { throw 'Retrieval did not survive recreation' }
```

Both the ledger and retrieval workflow should continue against the retained
PostgreSQL database. `docker compose stop` pauses the stack, and
`docker compose up -d` starts it again. `docker compose down` retains the named
volume; **adding `-v` deletes the database volume**. Keep independent backups
before loading documents you cannot afford to lose.

## 8. Add model-generated answers when you are ready

Use [Managing keys and secrets](managing-key-and-secrets.md) for provider setup,
Docker secret mounts, verification and credential rotation.

First verify that retrieval supplies the right evidence. A more expensive
model will not reliably repair a missing document or incorrect collection
binding. When you want generation:

1. Choose a provider and model tier, with explicit spending and output limits.
   Review the [provider examples](../../runbooks/providers/) and current
   provider configuration details in the developer guide.
2. Supply the provider credential through the Server's environment or supported
   secret-reference mechanism. Keep the actual key out of shapes, runbooks,
   images and source control. Recreate the Server if you change its environment.
3. Apply your reviewed provider configuration and configure the runbook's model
   routing to use it. Publish a new runbook version if that document changes,
   and create a new session for the new version.
4. Inspect the configured provider/model mapping before requesting a completed
   turn. Enable completion deliberately; it can send retrieved document text
   to the configured provider and incur charges. Model-probing diagnostics can
   also make paid requests.
5. Evaluate the returned completion text, citations, resolved provider/model and
   token usage. Compare several answerable and unanswerable questions against
   a reviewed key before using the result in your application.

The main walkthrough stops at verified retrieval. Follow
[Creating a laboratory](creating-a-lab.md) to improve your shapes and runbooks
for real corpora, and [Retrieval sizing](retrieval-sizing.md) to choose candidate,
context and output limits appropriate to those corpora.

## Common first-run problems

| Symptom | What to check |
|---|---|
| Docker cannot run the image | Use Linux containers; the release supports AMD64 and ARM64. |
| A host port is already allocated | Change the host-side Compose mapping and the corresponding client URL. |
| Server requests Azure storage settings | Set `MUNARIUM_SOURCE_STORE=pg` for this all-PostgreSQL example. |
| Database authentication fails after editing `.env` | An existing PostgreSQL volume retains its original role password. Reconcile the password and URI; do not delete data to repair credentials. |
| A request reports `uid-required` or fails authentication | Send the bearer token and caller UID; keep the same tenant throughout the workflow. |
| The runbook validates but retrieves nothing | Check upload prefixes, media types, bound collections, run-step results and approved cutover. |
| A collection appears in `skipped` | Inspect whether it has an active, available index before changing the question or model. |
| A changed runbook seems to have no effect | Check the session's pinned runbook version and the active indexes; use a fresh session for the intended version. |

## Continue from a working baseline

Keep the small sample working while adding your own document family. Replace
the fictional corpus with a representative slice, inspect extraction, and
build an answer key before tuning. Add access levels and compartments when
your application needs them, and validate that restricted callers receive only
their permitted evidence.

Use the [developer guide](dev-guide.md) for the full application and deployment
model, the [REST reference](../api/rest.md) for request contracts, and the
[official clients](../../../clients/README.md) when integrating an application.
For production, plan verified TLS, private credentials, backups, monitoring,
capacity and tested upgrade/rollback procedures in addition to the local
functionality demonstrated here.
