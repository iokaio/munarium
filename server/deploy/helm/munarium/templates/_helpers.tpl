{{/* SPDX-License-Identifier: Apache-2.0 */}}
{{/*
The MUNARIUM_REPLICA_COUNT value to render, or "" to render none (SCALE-1).

providerReplicaCount omitted, null or "": "" -- the v1.4 chart rendered no
entry, so an upgrade with the original values, and any count supplied by a
post-render or `kubectl set env` override, keeps its effect.
"auto": the Server processes the chart runs: the serving replicas plus, when
the optional builder group is enabled (SCALE-2), the builder replicas. A
serving pod that also builds (through an override) is one process and counts
once. A zero-replica deployment runs no process, and the count is still at
least 1.
A positive integer: that fixed divisor, even when it differs from replicas.
Anything else fails the render rather than starting pods that refuse to boot.
Every Server group renders the same value.
*/}}
{{- define "munarium.providerReplicaCount" -}}
{{- $v := .Values.providerReplicaCount -}}
{{- if or (kindIs "invalid" $v) (and (kindIs "string" $v) (eq $v "")) -}}
{{- else if and (kindIs "string" $v) (eq $v "auto") -}}
{{- $builders := 0 -}}
{{- if include "munarium.builderEnabled" . -}}
{{- $builders = include "munarium.builderReplicas" . | int -}}
{{- end -}}
{{- max 1 (add (int .Values.replicas) $builders) -}}
{{- else if regexMatch "^[1-9][0-9]*$" (toString $v) -}}
{{- toString $v -}}
{{- else -}}
{{- fail (printf "providerReplicaCount must be omitted, \"auto\" or a positive integer; got %v" $v) -}}
{{- end -}}
{{- end -}}

{{/*
"true" when the optional builder group is enabled (SCALE-2), else "".
Values carried from a v1.4 release (`--reuse-values`) have no `builder` key,
which reads as disabled; so does `builder: null`.
*/}}
{{- define "munarium.builderEnabled" -}}
{{- $b := .Values.builder -}}
{{- if and (kindIs "map" $b) (eq (toString (index $b "enabled")) "true") -}}
true
{{- end -}}
{{- end -}}

{{/* Reused v1.4 values have no defaults for a newly enabled builder group. */}}
{{- define "munarium.builderReplicas" -}}
{{- $replicas := dig "replicas" 1 .Values.builder -}}
{{- if not (regexMatch "^[0-9]+$" (toString $replicas)) -}}
{{- fail (printf "builder.replicas must be a non-negative integer; got %v" $replicas) -}}
{{- end -}}
{{- int $replicas -}}
{{- end -}}

{{/*
The environment every Server group shares: store, database, auth, logging,
the provider replica count, source store, Matrix and document intelligence.
The serving Deployment renders exactly these entries, as the v1.4 chart did;
the builder group appends its role settings.
*/}}
{{- define "munarium.serverEnv" -}}
- { name: MUNARIUM_STORE, value: postgres }
- name: MUNARIUM_DATABASE_URL
  valueFrom:
    secretKeyRef: { name: munarium-cell-a-app, key: uri } # CNPG-generated app secret
- { name: MUNARIUM_AUTH_MODE, value: static }
- { name: MUNARIUM_STATIC_TOKENS, value: {{ .Values.staticTokens | quote }} }
- { name: MUNARIUM_LOG, value: info }
{{- if (.Values.mcp | default dict).generationPolicy }}
- { name: MUNARIUM_MCP_GENERATION_POLICY, value: /etc/munarium/mcp-generation/policy.json }
{{- end }}
{{- $replicaCount := include "munarium.providerReplicaCount" . }}
{{- if $replicaCount }}
# providerReplicaCount (SCALE-1): divides each provider RPM/TPM
# limit and arms the cluster startup checks. Absent when the value
# is omitted, as in the v1.4 chart.
- { name: MUNARIUM_REPLICA_COUNT, value: {{ $replicaCount | quote }} }
{{- end }}
{{- if .Values.sourceStore.account }}
- { name: MUNARIUM_SOURCE_STORE, value: az }
- { name: MUNARIUM_AZURE_STORAGE_ACCOUNT, value: {{ .Values.sourceStore.account | quote }} }
- { name: MUNARIUM_AZURE_BLOB_CONTAINER, value: {{ .Values.sourceStore.container | quote }} }
- { name: MUNARIUM_BLOB_AUTH, value: managed_identity }
- { name: MUNARIUM_AZURE_CLIENT_ID, value: {{ .Values.workloadIdentity.clientId | quote }} }
{{- else }}
# No blob account configured: keep document bytes in Postgres so a
# plain `helm install` works with no Azure.
- { name: MUNARIUM_SOURCE_STORE, value: pg }
{{- end }}
{{- if .Values.matrix.baseUrl }}
- { name: MUNARIUM_MATRIX_BASE_URL, value: {{ .Values.matrix.baseUrl | quote }} }
{{- end }}
{{- if .Values.matrix.adminUrl }}
- { name: MUNARIUM_MATRIX_ADMIN_URL, value: {{ .Values.matrix.adminUrl | quote }} }
{{- end }}
{{- if .Values.docIntel.provider }}
- { name: MUNARIUM_DOCINTEL, value: {{ .Values.docIntel.provider | quote }} }
- { name: MUNARIUM_DOCINTEL_ENDPOINT, value: {{ .Values.docIntel.endpoint | quote }} }
- { name: MUNARIUM_DOCINTEL_AUTH, value: managed_identity }
{{- end }}
{{- end -}}

{{- define "munarium.mcpPolicyMount" -}}
- { name: mcp-generation-policy, mountPath: /etc/munarium/mcp-generation, readOnly: true }
{{- end -}}

{{- define "munarium.mcpPolicyVolume" -}}
- name: mcp-generation-policy
  configMap:
    name: munarium-mcp-generation-policy
{{- end -}}
