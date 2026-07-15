FROM sysg-usecase-base

COPY tests/docker/usecase/start-ambiguous-selector/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/start-ambiguous-selector/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
