FROM sysg-usecase-base

COPY tests/docker/usecase/status-no-supervisor-offline/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/status-no-supervisor-offline/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
