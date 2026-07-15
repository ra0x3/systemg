FROM sysg-usecase-base

COPY tests/docker/usecase/stop-ambiguous-selector/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/stop-ambiguous-selector/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
