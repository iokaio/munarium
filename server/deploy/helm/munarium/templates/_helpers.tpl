{{/* SPDX-License-Identifier: Apache-2.0 */}}
{{/*
The MUNARIUM_REPLICA_COUNT value to render, or "" to render none (SCALE-1).

providerReplicaCount omitted, null or "": "" -- the v1.4 chart rendered no
entry, so an upgrade with the original values, and any count supplied by a
post-render or `kubectl set env` override, keeps its effect.
"auto": the serving replica count. The chart has one Server group, so every
process both serves and may build; it is counted once. A zero-replica
deployment runs no process, and the count is still at least 1.
A positive integer: that fixed divisor, even when it differs from replicas.
Anything else fails the render rather than starting pods that refuse to boot.
*/}}
{{- define "munarium.providerReplicaCount" -}}
{{- $v := .Values.providerReplicaCount -}}
{{- if or (kindIs "invalid" $v) (and (kindIs "string" $v) (eq $v "")) -}}
{{- else if and (kindIs "string" $v) (eq $v "auto") -}}
{{- max 1 (int .Values.replicas) -}}
{{- else if regexMatch "^[1-9][0-9]*$" (toString $v) -}}
{{- toString $v -}}
{{- else -}}
{{- fail (printf "providerReplicaCount must be omitted, \"auto\" or a positive integer; got %v" $v) -}}
{{- end -}}
{{- end -}}
