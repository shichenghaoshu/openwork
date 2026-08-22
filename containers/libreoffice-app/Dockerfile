FROM python:3.12-slim-bookworm@sha256:a116514e19457bcb7af7efe9c3dd0b9b71e85b317694e7882a1c52aa15a78134

ARG CLI_ANYTHING_COMMIT=810c18b0d1ab9b234bc996c9fd999318523a3ef0

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        libreoffice-calc \
        libreoffice-impress \
        libreoffice-writer \
        fonts-dejavu-core \
        fonts-liberation \
    && rm -rf /var/lib/apt/lists/* \
    && python -m pip install --no-cache-dir \
        click==8.3.0 \
        defusedxml==0.7.1 \
        prompt-toolkit==3.0.52 \
    && python -m pip install --no-cache-dir --no-deps \
        "https://github.com/HKUDS/CLI-Anything/archive/${CLI_ANYTHING_COMMIT}.tar.gz#subdirectory=libreoffice/agent-harness"

COPY libreoffice_capability.py /opt/openwork/bin/libreoffice-capability
RUN chmod 0555 /opt/openwork/bin/libreoffice-capability \
    && mkdir -p /workspace/input /workspace/output \
    && chown 65534:65534 /workspace/output

USER 65534:65534
WORKDIR /workspace
ENTRYPOINT ["/opt/openwork/bin/libreoffice-capability"]
