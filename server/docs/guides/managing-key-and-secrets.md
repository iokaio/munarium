# Managing keys and secrets

This guide explains how to supply, verify, rotate and revoke credentials for
Munarium Server, including AI provider keys, PostgreSQL access, API bearer tokens
and capability signing keys. It follows the configuration implemented in
[Server configuration](../../src/munarium-server/src/config.rs) and the
[provider gateway](../../src/munarium-providers/src/lib.rs).

For a first deployment, complete [getting started](getting-started.md). The
Docker examples below modify that guide's `compose.yaml`, in the same private
deployment directory, using PowerShell 7 and Docker Desktop with Linux containers.
Keep the existing PostgreSQL volume and project name when making these changes.

## 1. Know which credential does which job

| Credential | Consumer | Supported input | When changes take effect |
|---|---|---|---|
| AI provider key | Server's outbound model requests | `ProviderConfig.spec.credentialRef` with an environment-variable name or file path | Resolved at use time; a changed container environment requires recreation |
| PostgreSQL connection string | Server's database pool | `MUNARIUM_DATABASE_URL`, containing the actual connection string | Server startup; recreate after changing it |
| PostgreSQL initialization password | PostgreSQL image | `POSTGRES_PASSWORD` or `POSTGRES_PASSWORD_FILE` | Initializes a new data directory; does not change an existing role password |
| Static API bearer tokens | Server's API authentication | `MUNARIUM_STATIC_TOKENS` or `MUNARIUM_STATIC_TOKEN_FILE` | Server startup |
| Capability JWT signing key | Server's capability-token issuer and verifier | `MUNARIUM_TOKEN_SECRET` or `MUNARIUM_TOKEN_SECRET_FILE` | Server startup |
| Object-storage and document-intelligence secrets | Server's selected backend | Backend-specific `*_REF` variables naming an environment variable or `file:/path` | These explicit references are resolved during configuration; recreate after rotation |

An AI provider key does not authenticate an application to Munarium. An API
bearer token does not authenticate Munarium to PostgreSQL. Give each purpose a
separate credential, and separate development, laboratory and production secrets.

For the static-token and signing-key pairs, the direct environment value takes
precedence over the file option. Remove the direct variable when switching to a
file; leaving an empty value is not the same as unsetting it.

## 2. Choose a supported AI provider

These are the provider families implemented by this Server, not a guarantee that
every model or account entitlement is available through each provider.

| `spec.provider` | Default endpoint | Default key variable | Capabilities |
|---|---|---|---|
| `anthropic` | `https://api.anthropic.com` | `MUNARIUM_SECRET_ANTHROPIC` | Messages/completion; no embeddings adapter |
| `openai` | `https://api.openai.com/v1` | `MUNARIUM_SECRET_OPENAI` | Chat completions and embeddings |
| `openrouter` | `https://openrouter.ai/api/v1` | `MUNARIUM_SECRET_OPENROUTER` | OpenAI-compatible adapter; model and endpoint capabilities still need verification |

Create the key in the provider account that should own the usage. Record its
owner, environment, purpose, expiry if applicable and provider-side usage limits
in your operations inventory. Record the secret's reference there, not its value.
Select models your account can actually use; inspect the deployed Server's tier
mapping rather than assuming a newer model name is present in an older image.

An applied provider configuration can override `spec.endpoint`. The `openai`
adapter appends `/chat/completions`, `/embeddings` and `/models` to its base URL
and uses bearer authentication. The Anthropic adapter appends `/v1/messages`
and `/v1/models`, using `x-api-key`. A vLLM service, enterprise gateway or
Azure-hosted endpoint must match the selected adapter's paths, request format
and authentication. An endpoint override alone does not add Azure deployment
URL rewriting, an `api-key` header or automatic Entra token refresh. Validate
compatibility for the endpoint you actually deploy. Never put credentials in
the endpoint URL.

The reserved provider selector `default` tries Anthropic, then OpenAI, then
OpenRouter, choosing the first family with a resolvable credential. Applied
tenant configurations take precedence over synthesized defaults within a family.
This checks local credential availability, not successful upstream authentication.
For predictable application routing and budgets, use an explicitly named provider
configuration and bind your runbook to it.

## 3. Choose how to deliver secrets

Environment injection is convenient for local work and supported by deployment
platforms. A host `.env` file supplies Compose interpolation values; it does not
automatically put every variable into the Server container. Add each intended
variable to the service's `environment` mapping or an appropriate `env_file`.

For example, add this entry to the existing Server environment if choosing the
conventional OpenAI environment variable:

```yaml
MUNARIUM_SECRET_OPENAI: ${MUNARIUM_SECRET_OPENAI:?Supply the OpenAI key privately}
```

Populate it from a private secret store or a hidden PowerShell prompt before
running Compose. Do not type the literal key into a command saved in shell history:

```powershell
$env:MUNARIUM_SECRET_OPENAI = Read-Host -AsSecureString 'OpenAI API key' |
    ConvertFrom-SecureString -AsPlainText
try {
    docker compose config --quiet
    if ($LASTEXITCODE -ne 0) { throw 'Compose validation failed' }
    docker compose up -d --no-deps --force-recreate server
    if ($LASTEXITCODE -ne 0) { throw 'Server recreation failed' }
} finally {
    Remove-Item Env:MUNARIUM_SECRET_OPENAI -ErrorAction SilentlyContinue
}
```

Supply the variable again on subsequent Compose invocations that require it.
Clearing the host variable does not remove the value from the running container,
and converting a secure prompt to plain text is necessary here to pass the value
to the process; it is not a guarantee that process memory is erased afterward.

Mounted files avoid putting provider key values in the container environment.
Compose declares each secret at the top level and grants it to individual
services, which receive it under `/run/secrets/`. Local file-backed Compose
secrets are bind mounts, not an encrypted vault. Protect the source files and
Docker access on the host. See [Docker's Compose secrets instructions](https://docs.docker.com/compose/how-tos/use-secrets/).

Keep secret files outside source control and image build contexts. Restrict their
Windows filesystem permissions to the operator and identities required by Docker.
Do not share terminal transcripts, resolved Compose output, container environment
dumps or backups of the deployment directory without checking their contents.
`docker compose config --quiet` validates without printing resolved values.

## 4. Mount an AI key and apply its provider configuration

Choose this file-based approach instead of the environment example when you want
an explicit provider reference backed by a mounted secret. In your private
deployment directory, create a secret file without echoing its value:

```powershell
New-Item -ItemType Directory -Path ./secrets -Force | Out-Null
if (Test-Path ./secrets/openai.txt) { throw 'Existing key file: follow the rotation procedure' }
$providerKey = Read-Host -AsSecureString 'OpenAI API key' |
    ConvertFrom-SecureString -AsPlainText
try {
    if ([string]::IsNullOrWhiteSpace($providerKey)) { throw 'Key is empty' }
    Set-Content -LiteralPath ./secrets/openai.txt -Value $providerKey -NoNewline -Encoding utf8
} finally {
    $providerKey = $null
}
```

Add these entries to the existing Compose document. Merge them into its mappings;
do not create a second `services` or `secrets` key or replace the Server's existing
database environment, volumes and ports:

```yaml
services:
  server:
    secrets:
      - openai_key
secrets:
  openai_key:
    file: ./secrets/openai.txt
```

Validate and recreate the Server with `docker compose config --quiet` followed
by `docker compose up -d --no-deps --force-recreate server`. Verify readiness.
The published image runs as UID 65532; its process must be able to read the
mounted file. Verify readability through the provider listing below. The image
has no shell, so do not depend on `docker exec server sh` for setup or diagnosis.

Create `provider-openai.yaml` in the deployment directory:

```yaml
apiVersion: munarium.ioka.io/v1
kind: ProviderConfig
metadata:
  name: lab-openai
spec:
  provider: openai
  credentialRef:
    file: /run/secrets/openai_key
  budgets:
    rpm: 10
    dailyTokens:
      fast: 20000
      capable: 10000
      frontier: 0
```

These token ceilings are example laboratory policy, not dollar budgets. Choose
limits for your workload and provider pricing. An omitted tier cap is unlimited;
the example blocks completions requested with the `frontier` tier. Add `models.fast`, `models.capable`
and `models.frontier` overrides when you need to pin account-supported models.
Use the [provider examples](../../runbooks/providers/) for further configurations.

For an environment-backed configuration, replace the `credentialRef` mapping with
`credentialRef: { env: MUNARIUM_SECRET_OPENAI }`. The value is the variable's
name. For a file reference it is the Linux path inside the container, not the
Windows host path, and there is no `file:` prefix inside that YAML value.

Apply the YAML through the Server API using your tenant's **rw** token. This
stores the configuration and reference; do not insert the actual key into YAML.
The `mgmt` role does not authorize this write:

```powershell
$base = 'http://localhost:18080'
$apiToken = Read-Host -AsSecureString 'Munarium rw bearer token' |
    ConvertFrom-SecureString -AsPlainText
$headers = @{ Authorization = "Bearer $apiToken"; 'X-Munarium-Uid' = 'key-operator' }
try {
    Invoke-RestMethod -Method Post -Uri "$base/v1/providers" -Headers $headers `
        -ContentType 'text/yaml' -Body (Get-Content ./provider-openai.yaml -Raw)
    $inventory = Invoke-RestMethod -Uri "$base/v1/providers" -Headers $headers
    $inventory.providers | Select-Object name, provider, source, credential_ok, fast, capable, frontier
} finally {
    $headers.Clear()
    $apiToken = $null
}
```

Select `lab-openai` in your runbook's model routing. If the application permits
per-turn provider overrides, include the name in `models.allowOverrides`.
Publish the changed runbook and start a session using that version. Merely
applying a provider does not change every existing runbook or session.

Provider configurations are tenant-scoped, but mounted files and the process
environment are shared by that Server process. A reference is not a vault access
policy. Treat permission to apply provider configurations as trusted operational
access; use separate deployments and secret grants where tenants require a
stronger credential boundary. Only mount keys that deployment needs.

## 5. Verify without confusing availability with validity

Start with `/healthz` and `/readyz`, then authenticated `GET /v1/providers`.
The provider listing makes no upstream call. `credential_ok: true` means the
reference resolves to a nonempty value; even a revoked or incorrect key can
pass this check. `false` means investigate the variable name, mount path,
readability or empty file. Applying YAML successfully is not a key validation.

Next, deliberately test the named configuration's `/v1/providers/{name}/health`
endpoint. It contacts the provider's models endpoint, so network, authentication
and endpoint compatibility become relevant. Then run a small, explicitly bounded
completion through the named provider or your application. Verify the selected
model, answer, usage and failure behavior. Test embeddings separately if used;
successful completion does not prove embedding access.

`GET /healthai` is a different diagnostic: it runs completion probes across the
three built-in tiers for each available conventional environment key. It can
make nine paid requests, and does not validate a custom file-backed configuration
or its endpoint. It also does not go through the named configuration's normal
completion budget path. Keep it out of routine liveness/readiness polling.

Set provider-account limits as well as Munarium's per-config rate and token
limits. Token limits are not monetary limits, and credentials reused by another
application, embedding requests and diagnostic calls need their own accounting.
Daily tier caps apply when a completion request names a tier; an explicit-model
request without a tier bypasses those caps. Enforce the intended request shape
at your application/API boundary rather than treating this example as a universal
spending limit.
Use the [token-budget guide](../tokenbudgets.md) and cost reports to monitor the
application you actually run.

## 6. Rotate or revoke AI provider keys

For planned rotation, create a replacement key with the intended account,
permissions and limits. Keep the old key valid during the change if the provider
allows overlap. Update the private environment source or mounted secret file.

Environment changes require recreating every Server replica. Editing a host
`.env` file or running `docker compose restart` does not replace a container's
environment. File credentials are read at use time, but a bind mount can retain
an old file after an editor or vault agent replaces that file. For local Docker
Desktop, recreate the Server after replacing the file and verify the new mount.
Do not assume the host edit reached a running container.

With the same reference name or path, no provider YAML change is needed. If the
reference changes, reapply the configuration and verify each replica after its
registry refresh. Check local resolution, then a small real request using the new
key. Revoke the old key at the provider after verification, and record the change
without recording either value. A config update or container restart does not
revoke the key at its issuer.

For a suspected leak, revoke the exposed key promptly at the provider, accepting
the resulting service interruption. Replace it, inspect provider usage and the
relevant logs for exposure, and remove leaked copies from their actual locations.
Deleting a file from Git's current tree does not remove earlier copies or make
the credential invalid. Do not restore a compromised key as a rollback step.

## 7. Manage PostgreSQL access and password changes

The persistent examples use `MUNARIUM_STORE=postgres`,
`MUNARIUM_SOURCE_STORE=pg` and a complete `MUNARIUM_DATABASE_URL`. The latter
contains the database password. Percent-encode reserved characters in the URI's
username/password components; the getting-started guide generates a hexadecimal
password to avoid that issue. Within the Compose network use `postgres:5432`;
`localhost` inside the Server container refers to that container itself.

**The Server does not implement `MUNARIUM_DATABASE_URL_FILE` or a database
password `*_REF` option.** Inject the actual URI into `MUNARIUM_DATABASE_URL`
through the deployment platform's environment facilities. The published image
has no shell to translate a mounted file through an improvised shell entrypoint.
Do not assume that mounting a URI secret automatically configures the pool.

The PostgreSQL image separately supports `POSTGRES_PASSWORD_FILE`. To use it,
replace its `POSTGRES_PASSWORD` environment entry with
`POSTGRES_PASSWORD_FILE: /run/secrets/postgres_password`, declare a top-level
file-backed secret, and grant that secret to the `postgres` service. This only
changes how PostgreSQL receives its initialization password; the Server still
needs the matching URI. These initialization settings do not change passwords
inside an existing database. See the [PostgreSQL image documentation](https://hub.docker.com/_/postgres).

For an existing database, plan a maintenance window for a single-role rotation:

1. Verify a recent backup and retain a working administrative connection. Record
   the role, database, consuming services and rollback procedure without passwords.
2. Pause writers and stop the Server service; retain the PostgreSQL service and
   its volume. Do not use `docker compose down -v` to fix authentication.
3. Connect as a PostgreSQL administrator and change the actual role password.
   For the getting-started deployment, start `docker compose exec postgres psql
   -U munarium -d munarium`, then use the interactive `\password munarium`
   command. It prompts for the replacement without putting a cleartext password
   into SQL command history. Use your own role and connection for external
   databases. See [psql's password command](https://www.postgresql.org/docs/16/app-psql.html).
4. Update the Server's private URI source and the matching initialization
   password source for future recovery/recreation. Recreate the Server so its
   pool uses the new credential. A host-file change alone is insufficient.
5. Verify readiness, read a previously stored fact through the authenticated
   Server API, and create/read a new test fact through that API. Exercise document
   retrieval too if source bytes are in PostgreSQL. This checks the application's
   database access rather than merely whether PostgreSQL accepts connections.
6. Confirm a new TCP login with the old password fails. Existing sessions may
   remain connected after a password change; handle those separately if revocation
   of active access is required. Resume writers after validation.

`pg_isready` alone does not prove that the application's password works. Neither
does a local administrative socket connection, which may use different
authentication. Use a fresh application connection for acceptance.

The local image's initialization role is highly privileged. For a managed or
shared database, have the DBA assign the permissions Munarium needs, including
startup migrations and pgvector setup, and validate those permissions in staging.
Do not assume a read/write-only role can run migrations. Use verified TLS for
external PostgreSQL; follow the certificate and connection instructions in the
[developer guide](dev-guide.md#10-ci-and-the-path-to-production). Back up data and manage
the credential inventory separately: a database dump is not a vault backup.

## 8. Manage Munarium API tokens and the signing secret

Replace public example tokens before giving others access. Under static
authentication, entries have the form `token:tenant:role`, comma-separated, with
roles `rw`, `ro` or `mgmt`. Generate independent random token values without
commas or colons. A file selected by `MUNARIUM_STATIC_TOKEN_FILE` contains the
same list; grant its Compose secret only to the Server and remove
`MUNARIUM_STATIC_TOKENS` when switching to that file.

The capability signing key must be at least 32 bytes. Generate a cryptographically
random value and keep it separate from bearer tokens and provider keys. Set
`MUNARIUM_TOKEN_SECRET_FILE` to the mounted file path, removing the direct
`MUNARIUM_TOKEN_SECRET` value when using that method. Preserve the signing key
across ordinary recreations if outstanding capability tokens should remain valid.

Both files are read at startup; replacing their contents requires a Server
restart/recreation on every replica. For planned static-token rotation, temporarily
configure both old and new entries for the same tenant/role, restart, move callers
to the new token, then remove the old entry and restart again. Verify the old
token is rejected. Removing a static token does not automatically revoke capability
JWTs already minted through it; use capability revocation or expiry as appropriate.

Changing the signing key invalidates outstanding capability JWTs once all replicas
use the replacement. The Server does not provide a dual-signing-key overlap
window. Coordinate the change and token reissuance; replicas with different keys
can produce intermittent authentication failures. See the
[security posture](../security-posture.md) for the role and capability boundaries.

## 9. Other backend secrets

| Backend | Secret configuration |
|---|---|
| Azure Blob with SAS | `MUNARIUM_BLOB_AUTH=sas` and `MUNARIUM_BLOB_SAS_REF` |
| S3 with static credentials | `MUNARIUM_S3_ACCESS_KEY_ID` together with `MUNARIUM_S3_SECRET_KEY_REF`; supply both or neither |
| GCS with explicit service-account JSON | `MUNARIUM_GCS_CREDENTIALS_REF` |
| Azure Document Intelligence with a key | `MUNARIUM_DOCINTEL_AUTH=key` and `MUNARIUM_DOCINTEL_KEY_REF` |

For these `*_REF` variables, the value is either an environment-variable name
such as `STORAGE_SECRET` or a string such as `file:/run/secrets/storage_key`.
This syntax differs from the provider YAML's `{ file: /run/secrets/openai_key }`.
Never put the secret value into the reference variable itself. Backend selection,
account, bucket and endpoint settings are also required; follow
[source stores](source-stores.md) or [document intelligence](document-intelligence.md).

Use a platform identity where the backend supports it, instead of creating
another long-lived key. Munarium does not fetch an arbitrary vault URI supplied
as a reference: the platform must deliver the value as environment or a file.
For explicit references resolved at startup, recreate the Server after replacing
the secret and verify an actual backend operation, such as source upload/readback.
Keep SAS tokens and other credentials out of recorded source URLs.

## 10. Troubleshoot and retain useful evidence

| Symptom | Check and next action |
|---|---|
| `credential_ok: false` | Check the exact container variable/path, empty content and UID 65532 readability; mount the secret and recreate |
| `credential_ok: true`, upstream authentication fails | Check key validity, account entitlement, endpoint and auth dialect; local resolution does not contact the provider |
| Newly mounted key is ignored | Inspect the applied config and runbook selection; a default selector or different config may reference another key |
| File change did not take effect | Recreate to refresh a replaced file's bind mount; restart all consumers that read secrets only at startup |
| `/healthai` skips a working custom provider | It probes conventional environment defaults, not the named file-backed config; test that config explicitly |
| PostgreSQL fails after a `.env` edit | Reconcile the actual role password with the Server URI; preserve the volume |
| File-based API/signing secret seems ignored | Unset the corresponding direct environment value, which takes precedence |
| Rotation works on only some requests | Verify every replica received the change and has restarted where required |

Retain the image digest, configuration names, secret version identifiers,
verification time, replica coverage and test outcomes. Record whether a test
only resolved a local reference or actually authenticated upstream. Exclude key
values, connection strings containing passwords, authorization headers and secret
file contents. Review error output before sharing it: malformed static-token
configuration can be included in startup errors.

After each change, the acceptance record should show the intended application
still works, the old credential is rejected where revocation was intended, data
survived recreation, and usage remains within the chosen policy.
