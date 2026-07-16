FROM sysg-usecase-base

# Real datastores — no stand-ins. Postgres and Redis are installed as ordinary
# binaries and driven by sysg (NOT by systemd/service scripts), so the harness
# exercises sysg supervising genuine stateful services.
RUN apt-get update && apt-get install -y \
    postgresql \
    postgresql-client \
    redis-server \
    curl \
    && rm -rf /var/lib/apt/lists/*

ENV PATH="/usr/lib/postgresql/14/bin:${PATH}"

COPY tests/docker/usecase/fullstack/base /usecase/base
COPY tests/docker/usecase/fullstack/patches /usecase/patches
COPY tests/docker/usecase/fullstack/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
