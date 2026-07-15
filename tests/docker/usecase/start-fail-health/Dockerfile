FROM sysg-usecase-base

COPY tests/docker/usecase/start-fail-health/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/start-fail-health/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
