FROM sysg-usecase-base

COPY tests/docker/usecase/stop-descendant-tree/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/stop-descendant-tree/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
