{{- define "vp-fms-interceptor.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "vp-fms-interceptor.fullname" -}}
{{- if .Values.fullnameOverride }}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- include "vp-fms-interceptor.name" . }}
{{- end }}
{{- end }}

{{- define "vp-fms-interceptor.labels" -}}
app: {{ include "vp-fms-interceptor.fullname" . }}
app.kubernetes.io/name: {{ include "vp-fms-interceptor.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
helm.sh/chart: {{ .Chart.Name }}-{{ .Chart.Version | replace "+" "_" }}
{{- end }}

{{- define "vp-fms-interceptor.selectorLabels" -}}
app: {{ include "vp-fms-interceptor.fullname" . }}
{{- end }}