FROM sysg-usecase-base

COPY tests/docker/usecase/start-supervisor-dead-stale-pid/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/start-supervisor-dead-stale-pid/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
