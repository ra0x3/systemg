FROM sysg-usecase-base

COPY tests/docker/usecase/restart-reconcile-changed/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/restart-reconcile-changed/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
