FROM sysg-usecase-base

COPY tests/docker/usecase/start-frozen-supervisor-recovers/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/start-frozen-supervisor-recovers/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
