# Deterministic OpenAI-compatible mock LLM for local dev / e2e (no API key).
ARG PY_IMAGE=python:3.13.18-alpine3.24@sha256:7984cf56ceaddebe4fefeb75d64000b9f45d8b77b11a09b24524da6cdc3af0c8
FROM ${PY_IMAGE}
LABEL org.opencontainers.image.title="sentience-mock-llm" \
      org.opencontainers.image.description="Deterministic mock of the OpenAI chat completions API (dev/e2e only)" \
      org.opencontainers.image.source="https://github.com/Vimothy-s-Vestibule/sentience-workers"
COPY mock_llm.py /app/mock_llm.py
USER 65534:65534
ENV PORT=8080 PYTHONDONTWRITEBYTECODE=1
EXPOSE 8080
HEALTHCHECK --interval=5s --timeout=2s --retries=10 \
    CMD wget -q -O /dev/null "http://127.0.0.1:${PORT}/health" || exit 1
CMD ["python", "-u", "/app/mock_llm.py"]
