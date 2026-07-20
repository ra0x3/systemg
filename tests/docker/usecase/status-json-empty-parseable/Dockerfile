FROM sysg-usecase-base

COPY tests/docker/usecase/status-json-empty-parseable/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/status-json-empty-parseable/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
