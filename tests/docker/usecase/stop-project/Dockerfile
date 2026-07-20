FROM sysg-usecase-base

COPY tests/docker/usecase/stop-project/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/stop-project/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
