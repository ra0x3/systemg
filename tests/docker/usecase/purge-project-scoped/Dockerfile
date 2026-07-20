FROM sysg-usecase-base

COPY tests/docker/usecase/purge-project-scoped/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/purge-project-scoped/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
