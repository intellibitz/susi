{{- define "susi.name" -}}
{{- .Chart.Name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- define "susi.fullname" -}}
{{- printf "%s-%s" .Release.Name (include "susi.name" .) | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- define "susi.labels" -}}
app.kubernetes.io/name: {{ include "susi.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end -}}
