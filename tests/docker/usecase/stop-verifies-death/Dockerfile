FROM sysg-usecase-base

COPY tests/docker/usecase/stop-verifies-death/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/stop-verifies-death/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
