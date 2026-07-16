FROM sysg-usecase-base

COPY tests/docker/usecase/inspect-project-scoped/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/inspect-project-scoped/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
