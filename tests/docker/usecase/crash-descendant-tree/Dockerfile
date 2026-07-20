FROM sysg-usecase-base

COPY tests/docker/usecase/crash-descendant-tree/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/crash-descendant-tree/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
