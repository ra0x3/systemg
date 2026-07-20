FROM sysg-usecase-base

COPY tests/docker/usecase/restart-reconcile-added/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/restart-reconcile-added/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
