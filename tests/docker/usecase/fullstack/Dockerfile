FROM sysg-usecase-base

# Real datastores — no stand-ins. Postgres and Redis are installed as ordinary
# binaries and driven by sysg (NOT by systemd/service scripts), so the harness
# exercises sysg supervising genuine stateful services.
#
# DEBIAN_FRONTEND=noninteractive + a preset TZ keep tzdata (a postgres dep) from
# blocking the build on its interactive "Geographic area:" prompt.
ENV DEBIAN_FRONTEND=noninteractive
ENV TZ=Etc/UTC
RUN apt-get update && apt-get install -y --no-install-recommends \
    tzdata \
    postgresql \
    postgresql-client \
    redis-server \
    curl \
    ca-certificates \
    git \
    && rm -rf /var/lib/apt/lists/*

ENV PATH="/usr/lib/postgresql/14/bin:${PATH}"

COPY tests/docker/usecase/fullstack/base /usecase/base
COPY tests/docker/usecase/fullstack/patches /usecase/patches
COPY tests/docker/usecase/fullstack/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
