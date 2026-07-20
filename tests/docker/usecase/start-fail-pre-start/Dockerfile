FROM sysg-usecase-base

COPY tests/docker/usecase/start-fail-pre-start/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/start-fail-pre-start/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
