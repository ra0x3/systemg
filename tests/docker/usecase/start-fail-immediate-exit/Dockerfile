FROM sysg-usecase-base

COPY tests/docker/usecase/start-fail-immediate-exit/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/start-fail-immediate-exit/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
